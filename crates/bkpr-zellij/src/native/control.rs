//! Bkpr-owned CLI pipe; native SDK commands are dispatch receipts, not observed commits.
use super::*;

const MAX_CONTROL_BYTES: usize = 16 * 1024;

// Ordinary callbacks are acknowledged by native NoChange. Only pending
// permission readiness explicitly blocks/unblocks its pipe.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRequest {
    version: u32,
    request_id: String,
    session_id: String,
    client_id: u16,
    #[serde(default)]
    receiver_plugin_id: Option<u32>,
    method: String,
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "arguments", deny_unknown_fields)]
enum Operation {
    #[serde(rename = "pane.set_state")]
    PaneState(PaneState),
    #[serde(rename = "workspace.rename")]
    RenameWorkspace(RenameWorkspace),
    #[serde(rename = "workspace.close")]
    CloseWorkspace(WorkspaceTarget),
    #[serde(rename = "client.focus")]
    Focus(Focus),
    #[serde(rename = "client.mode")]
    Mode(Mode),
    #[serde(rename = "client.detach")]
    Detach(ClientTarget),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneState {
    pane_id: String,
    pinned: Option<bool>,
    borderless: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameWorkspace {
    workspace_id: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceTarget {
    workspace_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientTarget {
    client_id: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Focus {
    client_id: u16,
    pane_id: Option<String>,
    tab_id: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mode {
    client_id: u16,
    mode: String,
}

impl ControlRequest {
    fn targets(&self, session: Option<&str>, client: u16) -> bool {
        session == Some(self.session_id.as_str()) && self.client_id == client
    }

    fn client_method(&self) -> bool {
        matches!(
            self.method.as_str(),
            "client.ready" | "client.focus" | "client.mode" | "client.detach"
        )
    }

    fn operation(&self) -> Result<Operation, String> {
        if self.version != 1 || self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err("Unsupported control version or invalid request identity".into());
        }
        if !self.arguments.is_object() {
            return Err("Control arguments must be an object".into());
        }
        serde_json::from_value(serde_json::json!({
            "method": self.method, "arguments": self.arguments,
        }))
        .map_err(|error| format!("Invalid control operation: {error}"))
    }
}

fn pane_target(value: &str) -> Result<PaneId, String> {
    terminal_id(value)
        .map(PaneId::Terminal)
        .ok_or_else(|| "Expected terminal_N pane identity".into())
}

fn input_mode(value: &str) -> Result<InputMode, String> {
    // Use the SDK's own vocabulary, with ordinary CLI casing accepted.
    let value = value.to_ascii_lowercase();
    let name = match value.as_str() {
        "normal" => "Normal",
        "locked" => "Locked",
        "resize" => "Resize",
        "pane" => "Pane",
        "tab" => "Tab",
        "scroll" => "Scroll",
        "entersearch" => "EnterSearch",
        "search" => "Search",
        "renametab" => "RenameTab",
        "renamepane" => "RenamePane",
        "session" => "Session",
        "move" => "Move",
        "prompt" => "Prompt",
        "tmux" => "Tmux",
        _ => return Err("Unknown native input mode".into()),
    };
    serde_json::from_value(serde_json::Value::String(name.into()))
        .map_err(|error| error.to_string())
}

pub(crate) fn sidebar_control_pipe(frontend: &mut crate::Frontend, message: &PipeMessage) -> bool {
    let PipeSource::Cli(source) = &message.source else {
        return false;
    };
    if frontend.overlay_role
        || frontend.guide_role
        || frontend.focused_role
        || frontend.configuration.get("role").map(String::as_str) != Some("sidebar")
    {
        return false;
    }
    let Some(payload) = message
        .payload
        .as_deref()
        .filter(|payload| payload.len() <= MAX_CONTROL_BYTES)
    else {
        return false;
    };
    let Ok(request) = serde_json::from_str::<ControlRequest>(payload) else {
        return false;
    };
    if !request.client_method()
        || request.receiver_plugin_id != Some(frontend.id)
        || !request.targets(
            frontend.configuration.get("session_id").map(String::as_str),
            frontend.client_id,
        )
    {
        return false;
    }
    if frontend
        .control_receipts
        .iter()
        .any(|(pipe, id)| pipe == source && id == &request.request_id)
    {
        return false;
    }
    frontend
        .control_receipts
        .push_back((source.clone(), request.request_id.clone()));
    if frontend.control_receipts.len() > 128 {
        frontend.control_receipts.pop_front();
    }
    if request.method == "client.ready" {
        if request.version != 1
            || request.request_id.is_empty()
            || request.request_id.len() > 128
            || request
                .arguments
                .as_object()
                .is_none_or(|arguments| !arguments.is_empty())
        {
            control_ready_response(
                source,
                &request.request_id,
                Some("Invalid readiness request"),
            );
        } else if frontend.granted {
            control_ready_response(source, &request.request_id, None);
        } else if frontend.control_ready.len() >= 16 {
            control_ready_response(
                source,
                &request.request_id,
                Some("Too many pending readiness requests"),
            );
        } else {
            block_cli_pipe_input(source);
            frontend.control_ready.push((
                source.clone(),
                request.request_id,
                std::time::Instant::now(),
            ));
            // Re-request only the existing normal sidebar permissions. A real
            // PermissionRequestResult, never this request, authorizes dispatch.
            request_permission(crate::PERMISSIONS);
            set_timeout(5.0);
        }
        return false;
    }
    let result = if !frontend.granted {
        Err("Selected sidebar permissions have not been granted".into())
    } else {
        request
            .operation()
            .and_then(|operation| dispatch_client(frontend, operation))
    };
    let response = match result {
        Ok(()) => {
            serde_json::json!({"request_id": request.request_id, "outcome": "dispatched", "result": {"receipt": "selected sidebar native SDK dispatch; completion not observed"}, "error": null})
        }
        Err(error) => {
            serde_json::json!({"request_id": request.request_id, "outcome": "failed", "result": null, "error": error})
        }
    };
    cli_pipe_output(source, &format!("{response}\n"));
    false
}

fn control_ready_response(source: &str, request_id: &str, error: Option<&str>) {
    let response = if let Some(error) = error {
        serde_json::json!({"request_id": request_id, "outcome": "failed", "result": {"ready": false}, "error": error})
    } else {
        serde_json::json!({"request_id": request_id, "outcome": "completed", "result": {"ready": true}, "error": null})
    };
    cli_pipe_output(source, &format!("{response}\n"));
}

pub(crate) fn settle_control_ready(frontend: &mut crate::Frontend, error: Option<&str>) {
    for (source, request_id, _) in frontend.control_ready.drain(..) {
        control_ready_response(&source, &request_id, error);
        acknowledge_cli(&source);
    }
}

pub(crate) fn expire_control_ready(frontend: &mut crate::Frontend) {
    if frontend.control_ready.is_empty() {
        return;
    }
    let now = std::time::Instant::now();
    let mut index = 0;
    while index < frontend.control_ready.len() {
        if now
            .duration_since(frontend.control_ready[index].2)
            .as_secs()
            >= 5
        {
            let (source, request_id, _) = frontend.control_ready.swap_remove(index);
            control_ready_response(
                &source,
                &request_id,
                Some("Normal sidebar permission readiness timed out"),
            );
            acknowledge_cli(&source);
        } else {
            index += 1;
        }
    }
}

fn control_client(expected: u16, actual: u16) -> Result<(), String> {
    if expected == actual {
        Ok(())
    } else {
        Err("Client target differs from selected sidebar replica".into())
    }
}

fn dispatch_client(frontend: &crate::Frontend, operation: Operation) -> Result<(), String> {
    match operation {
        Operation::Focus(target) => {
            control_client(frontend.client_id, target.client_id)?;
            let snapshot = frontend
                .snapshot
                .as_ref()
                .ok_or("Native membership snapshot unavailable")?;
            let (pane, tab) = match (target.pane_id, target.tab_id) {
                (Some(pane), None) => {
                    let membership = snapshot
                        .panes
                        .iter()
                        .find(|candidate| candidate.id == pane)
                        .ok_or("Terminal membership is not yet known")?;
                    let tab = membership
                        .tab_id
                        .strip_prefix("tab_")
                        .and_then(|tab| tab.parse::<usize>().ok())
                        .ok_or("Invalid native tab identity")?;
                    (pane_target(&pane)?, tab)
                }
                (None, Some(tab)) => {
                    let tab_id = format!("tab_{tab}");
                    let pane = snapshot
                        .panes
                        .iter()
                        .filter(|pane| pane.tab_id == tab_id)
                        .filter_map(|pane| pane_target(&pane.id).ok())
                        .find(|pane| {
                            get_pane_info(*pane).is_some_and(|info| {
                                !info.is_suppressed && !info.is_floating && info.is_selectable
                            })
                        })
                        .ok_or("Target tab has no eligible tiled terminal")?;
                    (pane, tab)
                }
                _ => return Err("Focus requires exactly one pane_id or tab_id".into()),
            };
            let tab = get_tab_info(tab).ok_or("Tab is no longer open")?;
            // Stack membership is not exposed by the baseline SDK. Focusing a
            // stacked pane can expand/focus it for every client viewing this tab.
            if tab
                .other_focused_clients
                .iter()
                .any(|client| *client != target.client_id)
            {
                return Err("Client-specific focus is unavailable while another client views the target tab".into());
            }
            if tab.is_fullscreen_active {
                return Err(
                    "Client-specific focus cannot change a tab's shared fullscreen state".into(),
                );
            }
            if tab.selectable_floating_panes_count > 0 {
                return Err(
                    "Client-specific focus cannot hide a tab's selectable floating panes".into(),
                );
            }
            let info = get_pane_info(pane).ok_or("Terminal is no longer open")?;
            if tab.is_fullscreen_active || info.is_fullscreen {
                return Err("Client-specific focus cannot change shared tiled fullscreen".into());
            }
            if info.is_suppressed || info.is_floating || !info.is_selectable {
                return Err(
                    "Client focus requires a live selectable unsuppressed tiled terminal".into(),
                );
            }
            focus_pane_with_id(pane, false, false);
        }
        Operation::Mode(target) => {
            control_client(frontend.client_id, target.client_id)?;
            switch_to_input_mode(&input_mode(&target.mode)?);
        }
        Operation::Detach(target) => {
            control_client(frontend.client_id, target.client_id)?;
            detach();
        }
        _ => return Err("Selected sidebar accepts only client control methods".into()),
    }
    Ok(())
}

impl Owner {
    pub(super) fn control_pipe(&mut self, message: &PipeMessage) -> bool {
        let PipeSource::Cli(source) = &message.source else {
            return false;
        };
        // Returning also acknowledges bootstrap, EOF and unmatched requests.
        let Some(payload) = message
            .payload
            .as_deref()
            .filter(|p| p.len() <= MAX_CONTROL_BYTES)
        else {
            return false;
        };
        let Ok(request) = serde_json::from_str::<ControlRequest>(payload) else {
            return false;
        };
        if request.method == "owner.discover" {
            if request.version != 1
                || request.request_id.is_empty()
                || request.request_id.len() > 128
                || request
                    .arguments
                    .as_object()
                    .is_none_or(|arguments| !arguments.is_empty())
                || self.configuration.get("session_id") != Some(&request.session_id)
            {
                return false;
            }
            if !self.granted {
                self.permission_requested = true;
                request_permission(crate::PERMISSIONS);
                return false;
            }
            if !self.context_ready {
                return false;
            }
            self.recover();
            if self.error.is_some()
                || self.model.is_none()
                || self
                    .control_receipts
                    .iter()
                    .any(|(pipe, id)| pipe == source && id == &request.request_id)
            {
                return false;
            }
            self.control_receipts
                .push_back((source.clone(), request.request_id.clone()));
            if self.control_receipts.len() > 128 {
                self.control_receipts.pop_front();
            }
            let response = serde_json::json!({
                "request_id": request.request_id,
                "outcome": "completed",
                "result": {"owner_plugin_id": self.id, "client_id": self.client_id},
                "error": null,
            });
            cli_pipe_output(source, &format!("{response}\n"));
            return false;
        }
        if !request.targets(
            self.configuration.get("session_id").map(String::as_str),
            self.client_id,
        ) || request.receiver_plugin_id != Some(self.id)
        {
            return false;
        }
        if request.client_method() {
            // Client-bound SDK actions execute only in the explicitly selected sidebar.
            return false;
        }
        // Pipe input can arrive in repeated callbacks. Never dispatch or print twice on that pipe.
        if self
            .control_receipts
            .iter()
            .any(|(pipe, id)| pipe == source && id == &request.request_id)
        {
            return false;
        }
        self.control_receipts
            .push_back((source.clone(), request.request_id.clone()));
        if self.control_receipts.len() > 128 {
            self.control_receipts.pop_front();
        }
        let result = if !self.granted {
            Err("Native owner permissions have not been granted".into())
        } else {
            self.recover();
            request
                .operation()
                .and_then(|operation| self.dispatch_control(operation))
        };
        let response = match result {
            Ok(()) => {
                serde_json::json!({"request_id": request.request_id, "outcome": "dispatched", "result": {"receipt": "native SDK dispatch; completion not observed"}, "error": self.error})
            }
            Err(error) => {
                serde_json::json!({"request_id": request.request_id, "outcome": "failed", "result": null, "error": error})
            }
        };
        cli_pipe_output(source, &format!("{response}\n"));
        false
    }

    fn dispatch_control(&mut self, operation: Operation) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if !self.context_ready || self.model.is_none() {
            return Err("Authoritative native workspace context is unavailable".into());
        }
        match operation {
            Operation::PaneState(state) => {
                let pane = pane_target(&state.pane_id)?;
                let info = get_pane_info(pane).ok_or("Terminal is no longer open")?;
                if info.is_suppressed {
                    return Err("Cannot change state of a suppressed terminal".into());
                }
                if state.pinned.is_none() && state.borderless.is_none() {
                    return Err("At least one pane state field is required".into());
                }
                if state.pinned.is_some() && !info.is_floating {
                    return Err("Pinned state requires a floating pane".into());
                }
                if let Some(pinned) = state.pinned {
                    set_floating_pane_pinned(pane, pinned);
                }
                if let Some(borderless) = state.borderless {
                    set_pane_borderless(pane, borderless);
                }
            }
            Operation::RenameWorkspace(target) => {
                self.action(
                    self.id,
                    Action::RenameWorkspace {
                        workspace_id: target.workspace_id,
                        label: target.name,
                    },
                    None,
                    None,
                )?;
                if let Some(model) = self.model.as_mut() {
                    model.revision += 1;
                }
                self.publish();
            }
            Operation::CloseWorkspace(target) => {
                self.action(
                    self.id,
                    Action::CloseWorkspace {
                        workspace_id: target.workspace_id,
                    },
                    None,
                    None,
                )?;
                if let Some(model) = self.model.as_mut() {
                    model.revision += 1;
                }
                self.update_host_context();
                self.publish();
            }
            Operation::Focus(_) | Operation::Mode(_) | Operation::Detach(_) => {
                return Err(
                    "Client control requires an explicitly selected sidebar replica".into(),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_authority_requires_both_session_and_client() {
        let request: ControlRequest = serde_json::from_str(r#"{"version":1,"request_id":"r","session_id":"s","client_id":7,"method":"client.detach","arguments":{"client_id":7}}"#).unwrap();
        assert!(request.targets(Some("s"), 7));
        assert!(!request.targets(Some("other"), 7));
        assert!(!request.targets(Some("s"), 8));
        assert!(!request.targets(None, 7));
    }

    #[test]
    fn control_does_not_admit_extra_mutation_arguments_or_nonobjects() {
        let mut request: ControlRequest = serde_json::from_str(r#"{"version":1,"request_id":"r","session_id":"s","client_id":7,"method":"client.detach","arguments":{"client_id":7,"all":true}}"#).unwrap();
        assert!(request.operation().is_err());
        request.arguments = serde_json::json!([]);
        assert!(request.operation().is_err());
    }

    #[test]
    fn nested_client_target_cannot_override_replica_authority() {
        assert!(control_client(7, 7).is_ok());
        assert!(control_client(7, 8).is_err());
    }

    #[test]
    fn bootstrap_eof_and_foreign_authority_do_not_mutate_or_queue() {
        let mut owner = Owner {
            client_id: 7,
            configuration: BTreeMap::from([("session_id".into(), "ours".into())]),
            ..Owner::default()
        };
        for payload in [
            None,
            Some(""),
            Some(
                r#"{"version":1,"request_id":"r","session_id":"foreign","client_id":7,"method":"client.detach","arguments":{"client_id":7}}"#,
            ),
            Some(
                r#"{"version":1,"request_id":"r","session_id":"ours","client_id":8,"method":"client.detach","arguments":{"client_id":8}}"#,
            ),
            Some(
                r#"{"version":1,"request_id":"r","session_id":"ours","client_id":7,"receiver_plugin_id":9,"method":"client.detach","arguments":{"client_id":7}}"#,
            ),
            Some(
                r#"{"version":1,"request_id":"discover","session_id":"ours","client_id":0,"method":"owner.discover","arguments":{}}"#,
            ),
        ] {
            owner.pipe(PipeMessage {
                source: PipeSource::Cli("control-pipe".into()),
                name: CONTROL.into(),
                payload: payload.map(str::to_owned),
                args: BTreeMap::new(),
                is_private: false,
            });
        }
        assert!(owner.queued.is_empty());
        assert!(!owner.granted);
        assert!(owner.control_receipts.is_empty());
    }

    #[test]
    fn unselected_sidebar_cannot_claim_a_client_request() {
        let mut frontend = crate::Frontend {
            id: 10,
            client_id: 7,
            configuration: BTreeMap::from([
                ("session_id".into(), "ours".into()),
                ("role".into(), "sidebar".into()),
            ]),
            ..crate::Frontend::default()
        };
        let message = PipeMessage {
            source: PipeSource::Cli("client-pipe".into()),
            name: CONTROL.into(),
            payload: Some(r#"{"version":1,"request_id":"r","session_id":"ours","client_id":7,"receiver_plugin_id":9,"method":"client.detach","arguments":{"client_id":7}}"#.into()),
            args: BTreeMap::new(),
            is_private: false,
        };
        sidebar_control_pipe(&mut frontend, &message);
        assert!(frontend.control_receipts.is_empty());
        assert!(!frontend.granted);
    }

    #[test]
    fn permission_readiness_timeout_releases_only_expired_requests_without_authorizing() {
        let now = std::time::Instant::now();
        let mut frontend = crate::Frontend {
            control_ready: vec![
                (
                    "expired".into(),
                    "old".into(),
                    now - std::time::Duration::from_secs(6),
                ),
                ("waiting".into(), "new".into(), now),
            ],
            ..crate::Frontend::default()
        };
        ACKNOWLEDGED_CLI.with(|acknowledged| acknowledged.borrow_mut().clear());
        expire_control_ready(&mut frontend);
        assert_eq!(frontend.control_ready.len(), 1);
        assert_eq!(frontend.control_ready[0].0, "waiting");
        assert!(!frontend.granted);
        ACKNOWLEDGED_CLI.with(|acknowledged| assert_eq!(*acknowledged.borrow(), ["expired"]));
        settle_control_ready(
            &mut frontend,
            Some("Normal sidebar permissions were denied"),
        );
        assert!(frontend.control_ready.is_empty());
        assert!(!frontend.granted);
    }
}
