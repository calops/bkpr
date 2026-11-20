//! Owner-scoped recovery facts and worker-only native resume admission.
use super::*;
use crate::metadata::{self, Paragraph};
use bkpr_tui_framework::search_list::{MouseActivation, SearchList};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Exact retained pane and raw native intent, never a title or a live capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    /// Retained source pane on the selected endpoint.
    pub(super) pane_id: String,
    /// Descriptor whose complete identity must still match before submission.
    pub(super) expected: HarnessRecovery,
}

impl From<&RecoveryCandidate> for Key {
    fn from(candidate: &RecoveryCandidate) -> Self {
        Self {
            pane_id: candidate.pane_id.clone(),
            expected: candidate.harness.clone(),
        }
    }
}

/// Only the native configuration policy understood by this client is admitted.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Policy {
    /// Resume uses the current native profile and permissions, not saved settings.
    CurrentNativeConfiguration,
}

/// Validated, unambiguous candidates published by one endpoint.
#[derive(Debug)]
pub(super) struct Listing {
    recoveries: Vec<RecoveryCandidate>,
    panes: HashMap<String, usize>,
}

impl Listing {
    fn parse(value: Value) -> Result<Self> {
        #[derive(Deserialize)]
        struct Reply {
            policy: Policy,
            recoveries: Vec<RecoveryCandidate>,
        }
        let reply: Reply = serde_json::from_value(value)
            .context("decoding current-native-configuration recovery list")?;
        let Policy::CurrentNativeConfiguration = reply.policy;
        let mut panes = HashMap::with_capacity(reply.recoveries.len());
        for (index, candidate) in reply.recoveries.iter().enumerate() {
            if candidate.pane_id.is_empty()
                || panes.insert(candidate.pane_id.clone(), index).is_some()
            {
                bail!("recovery list contains an empty or ambiguous duplicate pane identity");
            }
            match (
                &candidate.workspace_id,
                &candidate.window_id,
                candidate.window_revision,
            ) {
                (Some(workspace), Some(window), Some(_))
                    if !workspace.is_empty() && !window.is_empty() => {}
                (None, None, None) => {}
                _ => bail!("recovery list contains inconsistent pane membership"),
            }
        }
        Ok(Self {
            panes,
            recoveries: reply.recoveries,
        })
    }

    fn candidate(&self, key: &Key) -> Option<&RecoveryCandidate> {
        let candidate = &self.recoveries[*self.panes.get(&key.pane_id)?];
        (candidate.harness == key.expected).then_some(candidate)
    }

    fn eligible(&self, key: &Key) -> Result<&RecoveryCandidate> {
        let index = self
            .panes
            .get(&key.pane_id)
            .context("selected recovery no longer exists on this endpoint")?;
        let candidate = &self.recoveries[*index];
        if candidate.harness != key.expected {
            bail!("selected recovery descriptor changed; refresh and select it again");
        }
        if candidate.status != RecoveryStatus::Recoverable {
            bail!(
                "selected recovery is {}{}",
                status_label(candidate.status),
                candidate
                    .reason
                    .as_deref()
                    .map_or_else(String::new, |reason| format!(": {reason}"))
            );
        }
        Ok(candidate)
    }
}

/// Blocking cancellable observation, without cancelling the owner's attachment.
pub(super) fn fetch_cancellable(
    endpoint: &Endpoint,
    cancellation: &CancellationToken,
) -> Result<Listing> {
    Listing::parse(endpoint.checked_cancellable(Operation::RecoveryList, cancellation)?)
}

/// Confirmed process creation and projection, still awaiting authenticated binding.
#[derive(Debug)]
pub(super) struct Started {
    /// Authoritative registry returned by the single resume operation.
    pub(super) registry: RegistrySnapshot,
    /// Exact validated target, which may differ from the retained source pane.
    pub(super) selection: management::Selection,
    /// Original selected native intent, not proof the native conversation loaded.
    pub(super) source: Key,
}

/// Resume failure retaining whether the mutation method was invoked, not accepted.
#[derive(Debug)]
pub(super) struct ResumeFailure {
    /// True only once `Endpoint::checked(PaneResume)` was invoked; not dispatch proof.
    pub(super) attempted: bool,
    /// Original typed transport failure; unknown outcomes must never be replayed.
    pub(super) error: anyhow::Error,
}

impl std::fmt::Display for ResumeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, formatter)
    }
}

impl std::error::Error for ResumeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

/// Freshly recheck exact eligibility, then invoke one mutation. Cancellation before
/// that invocation prevents mutation; after invocation its receipt is drained.
/// The caller must separately enforce read-only policy and one admission per picker.
pub(super) fn resume(
    endpoint: &Endpoint,
    key: &Key,
    cancellation: &CancellationToken,
) -> std::result::Result<Started, ResumeFailure> {
    let before = |error| ResumeFailure {
        attempted: false,
        error,
    };
    if cancellation.is_cancelled() {
        return Err(before(anyhow::anyhow!(
            "recovery cancelled before resume submission"
        )));
    }
    let listing = fetch_cancellable(endpoint, cancellation).map_err(before)?;
    let candidate = listing.eligible(key).map_err(before)?;
    let operation = Operation::PaneResume {
        pane_id: key.pane_id.clone(),
        expected: key.expected.clone(),
    };
    if cancellation.is_cancelled() {
        return Err(before(anyhow::anyhow!(
            "recovery cancelled before resume submission"
        )));
    }
    let value = endpoint.checked(operation).map_err(|error| ResumeFailure {
        attempted: true,
        error,
    })?;
    Started::parse(value, key, candidate).map_err(|error| ResumeFailure {
        attempted: true,
        error,
    })
}

impl Started {
    fn parse(value: Value, source: &Key, candidate: &RecoveryCandidate) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Binding {
            PendingNativeBinding,
        }
        #[derive(Deserialize)]
        struct Reply {
            policy: Policy,
            started: bool,
            binding: Binding,
            source_pane_id: String,
            #[serde(flatten)]
            result: RegistryResult,
        }
        (|| -> Result<Self> {
            let reply: Reply = serde_json::from_value(value)
                .context("resume response omitted a valid native registry result")?;
            let Policy::CurrentNativeConfiguration = reply.policy;
            let Binding::PendingNativeBinding = reply.binding;
            if !reply.started || reply.source_pane_id != source.pane_id {
                bail!("resume response did not confirm process creation for the selected source");
            }
            let result = reply.result;
            let pane = result.pane_id.context("resume response omitted pane_id")?;
            let workspace = result
                .workspace_id
                .context("resume response omitted workspace_id")?;
            let window = result
                .window_id
                .context("resume response omitted window_id")?;
            if pane.is_empty() || workspace.is_empty() || window.is_empty() {
                bail!("resume response returned an empty target identity");
            }
            if candidate
                .workspace_id
                .as_ref()
                .is_some_and(|id| id != &workspace)
                || candidate.window_id.as_ref().is_some_and(|id| id != &window)
            {
                bail!("resume response returned a foreign target workspace or window");
            }
            let registry = result.registry;
            let mut owners = registry
                .workspaces
                .iter()
                .filter(|entry| entry.id == workspace);
            let owner = owners
                .next()
                .context("resume response omitted target workspace")?;
            if owners.next().is_some() {
                bail!("resume response contains ambiguous target workspace identities");
            }
            let mut windows = registry
                .workspaces
                .iter()
                .flat_map(|entry| {
                    entry
                        .windows
                        .iter()
                        .map(move |window| (entry.id.as_str(), window))
                })
                .filter(|(_, entry)| entry.id == window);
            let (window_owner, document) = windows
                .next()
                .context("resume response omitted target window")?;
            if windows.next().is_some() || window_owner != owner.id {
                bail!("resume response contains foreign or ambiguous target window ownership");
            }
            let mut panes = registry.panes.iter().filter(|entry| entry.id == pane);
            let target = panes
                .next()
                .context("resume response omitted created pane")?;
            if panes.next().is_some()
                || target.workspace_id.as_deref() != Some(workspace.as_str())
                || target.window_id.as_deref() != Some(window.as_str())
                || target.status != PaneStatus::Running
            {
                bail!("resume response contains inconsistent target membership or execution state");
            }
            let total: usize = registry
                .workspaces
                .iter()
                .flat_map(|entry| &entry.windows)
                .map(|entry| projections(entry.layout.as_ref(), &pane))
                .sum();
            if projections(document.layout.as_ref(), &pane) != 1 || total != 1 {
                bail!("resume response did not confirm one exact target window projection");
            }
            Ok(Self {
                registry,
                selection: management::Selection {
                    workspace,
                    window,
                    pane,
                },
                source: source.clone(),
            })
        })()
        .map_err(transport::mark_outcome_unknown)
        .context(
            "resume succeeded but its target was not confirmed; outcome unknown, do not replay",
        )
    }
}

fn projections(layout: Option<&LayoutNode>, pane: &str) -> usize {
    match layout {
        None => 0,
        Some(LayoutNode::Leaf { pane_id }) => usize::from(pane_id == pane),
        Some(LayoutNode::Split { first, second, .. }) => {
            projections(Some(first), pane) + projections(Some(second), pane)
        }
    }
}

/// Application-owned label for native recovery admission, not semantic harness state.
pub(super) fn status_label(status: RecoveryStatus) -> &'static str {
    match status {
        RecoveryStatus::Live => "Live",
        RecoveryStatus::Recoverable => "Recoverable",
        RecoveryStatus::Unavailable => "Unavailable",
    }
}

/// Application-owned semantic ink for the recovery admission label.
pub(super) fn status_role(status: RecoveryStatus) -> Role {
    match status {
        RecoveryStatus::Live => Role::Info,
        RecoveryStatus::Recoverable => Role::Success,
        RecoveryStatus::Unavailable => Role::Warning,
    }
}

fn label(candidate: &RecoveryCandidate) -> String {
    metadata::clean(&format!(
        "{}  ·  {}  ·  {}  ·  {}",
        candidate.title,
        candidate.harness.kind.display_name(),
        status_label(candidate.status),
        candidate.harness.conversation_id,
    ))
}

/// Retained owner-independent search and cached selected details. No I/O occurs here.
pub(super) struct Picker {
    /// Generic list mechanics; row pointers select but never submit a resume.
    pub(super) list: SearchList<Key>,
    listing: Listing,
    readonly: bool,
    detail_key: Option<Key>,
    details: Vec<Paragraph>,
    details_valid: bool,
}

impl Picker {
    /// Create an empty-safe picker; read-only attachments can inspect but not resume.
    pub(super) fn new(listing: Listing, readonly: bool) -> Self {
        let mut list = SearchList::new(
            listing
                .recoveries
                .iter()
                .map(|candidate| (Key::from(candidate), label(candidate))),
            None,
        );
        list.set_mouse_activation(MouseActivation::Select);
        list.set_select_on_hover(false);
        list.set_layout(bkpr_tui_framework::search_list::ListLayout::Cards { details: true });
        Self {
            list,
            listing,
            readonly,
            detail_key: None,
            details: Vec::new(),
            details_valid: false,
        }
    }

    /// Exact selected retained candidate; Live never implies a different pane target.
    pub(super) fn selected(&self) -> Option<&RecoveryCandidate> {
        self.listing.candidate(self.list.selected()?)
    }

    /// Cached admission ink for an exact visible identity; stale keys remain muted.
    pub(super) fn row_status(&self, key: &Key) -> Role {
        self.listing
            .candidate(key)
            .map_or(Role::Muted, |candidate| status_role(candidate.status))
    }

    /// Local button eligibility only; the worker must still fetch fresh admission facts.
    pub(super) fn can_resume(&self) -> bool {
        !self.readonly
            && self
                .selected()
                .is_some_and(|candidate| candidate.status == RecoveryStatus::Recoverable)
    }

    /// Replace published facts while preserving query and exact selected native intent.
    pub(super) fn replace(&mut self, listing: Listing) {
        self.list.replace_items(
            listing
                .recoveries
                .iter()
                .map(|candidate| (Key::from(candidate), label(candidate))),
        );
        self.listing = listing;
        self.details_valid = false;
    }

    /// Cached metadata paragraphs, rebuilt only on selection or listing changes.
    pub(super) fn details(&mut self) -> &[Paragraph] {
        if !self.details_valid || self.detail_key.as_ref() != self.list.selected() {
            let details = self
                .selected()
                .map(|candidate| detail_paragraphs(candidate, self.readonly));
            self.detail_key = self.list.selected().cloned();
            self.details = details.unwrap_or_else(|| {
                vec![Paragraph::new(
                    "No matching saved conversations.",
                    Role::Muted,
                )]
            });
            self.details_valid = true;
        }
        &self.details
    }
}

fn detail_paragraphs(candidate: &RecoveryCandidate, readonly: bool) -> Vec<Paragraph> {
    let mut rows = vec![
        Paragraph::new(&candidate.title, Role::Title),
        Paragraph::new(
            format!("Harness: {}", candidate.harness.kind.display_name()),
            Role::Panel,
        ),
        Paragraph::new(
            format!("Conversation: {}", candidate.harness.conversation_id),
            Role::Panel,
        ),
        Paragraph::new(
            format!("Saved CWD: {}", candidate.harness.cwd.display()),
            Role::Panel,
        ),
        Paragraph::new(format!("Source pane: {}", candidate.pane_id), Role::Muted),
        Paragraph::new(
            format!("Status: {}", status_label(candidate.status)),
            status_role(candidate.status),
        ),
    ];
    if let Some(reason) = &candidate.reason {
        rows.push(Paragraph::new(format!("Reason: {reason}"), Role::Warning));
    }
    rows.push(Paragraph::new(match candidate.status {
        RecoveryStatus::Live => "This conversation is live or pending, possibly in another pane. No live navigation target is implied.",
        RecoveryStatus::Recoverable => "An explicit resume attempt is allowed; native session and provider availability are not preflighted.",
        RecoveryStatus::Unavailable => "Current execution, configuration or lifecycle state prevents a resume attempt.",
    }, Role::Muted));
    if readonly {
        rows.push(Paragraph::new(
            "Read-only attachment: listing is allowed; resume is disabled.",
            Role::Warning,
        ));
    }
    rows.push(Paragraph::new("Resume uses current native configuration and permissions, not saved argv, model, provider, approvals or sandbox settings.", Role::Warning));
    rows.push(Paragraph::new("Process creation is pending authenticated native binding, not proof that the conversation loaded.", Role::Muted));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        os::unix::{ffi::OsStringExt, net::UnixListener},
        thread,
    };

    fn candidate(status: RecoveryStatus) -> RecoveryCandidate {
        RecoveryCandidate {
            pane_id: "source".into(),
            workspace_id: Some("workspace".into()),
            window_id: Some("window".into()),
            window_revision: Some(7),
            title: "retained title".into(),
            harness: HarnessRecovery {
                kind: HarnessKind::Pi,
                conversation_id: "full-native-conversation-id".into(),
                cwd: "/saved/cwd".into(),
            },
            status,
            reason: None,
        }
    }

    fn listing(candidates: &[RecoveryCandidate]) -> Value {
        json!({"policy": "current_native_configuration", "recoveries": candidates})
    }

    fn response(value: Value) -> Response {
        Response {
            version: PROTOCOL_VERSION,
            ok: true,
            result: Some(value.into()),
            error: None,
            failure: None,
        }
    }

    fn started() -> Value {
        let registry = RegistrySnapshot {
            revision: 10,
            workspaces: vec![Workspace {
                id: "workspace".into(),
                name: "work".into(),
                revision: 3,
                windows: vec![Window {
                    id: "window".into(),
                    name: "terminal".into(),
                    revision: 8,
                    layout: Some(LayoutNode::Leaf {
                        pane_id: "target".into(),
                    }),
                }],
            }],
            panes: vec![PaneInfo {
                id: "target".into(),
                workspace_id: Some("workspace".into()),
                window_id: Some("window".into()),
                title: "new process".into(),
                status: PaneStatus::Running,
                exit_code: None,
                cwd: "/saved/cwd".into(),
                foreground_cwd: None,
                harness: None,
                harness_status: None,
                project: None,
                home: "/private/home".into(),
                isolated_environment: false,
                harness_status_changed_at_ms: None,
            }],
        };
        let mut value = serde_json::to_value(RegistryResult {
            registry,
            pane_id: Some("target".into()),
            workspace_id: Some("workspace".into()),
            window_id: Some("window".into()),
            created_panes: Default::default(),
        })
        .unwrap();
        value["policy"] = json!("current_native_configuration");
        value["started"] = json!(true);
        value["binding"] = json!("pending_native_binding");
        value["source_pane_id"] = json!("source");
        value
    }

    fn run(
        replies: Vec<Option<Response>>,
        key: &Key,
        cancellation: &CancellationToken,
    ) -> (std::result::Result<Started, ResumeFailure>, Vec<Request>) {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("recovery.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            replies
                .into_iter()
                .map(|reply| {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let request = transport::test_wire::read_request(&mut stream).unwrap();
                    if let Some(reply) = reply {
                        transport::test_wire::write_response(&mut stream, &reply).unwrap();
                    }
                    request
                })
                .collect()
        });
        let result = resume(&Endpoint::unmanaged(&socket), key, cancellation);
        (result, server.join().unwrap())
    }

    #[test]
    fn unsupported_policy_and_duplicate_source_ids_cannot_be_selected() {
        let first = candidate(RecoveryStatus::Recoverable);
        let mut second = first.clone();
        second.harness.conversation_id = "different-conversation".into();
        assert!(Listing::parse(listing(&[first.clone(), second])).is_err());
        let mut unsupported = listing(&[first]);
        unsupported["policy"] = json!("saved_native_configuration");
        assert!(Listing::parse(unsupported).is_err());
    }

    #[test]
    fn stale_absent_live_and_unavailable_selections_never_attempt_mutation() {
        let original = candidate(RecoveryStatus::Recoverable);
        let key = Key::from(&original);
        let mut changed_id = original.clone();
        changed_id.harness.conversation_id.push_str("-changed");
        let mut changed_kind = original.clone();
        changed_kind.harness.kind = HarnessKind::Omp;
        let mut changed_raw_cwd = original.clone();
        changed_raw_cwd.harness.cwd =
            std::ffi::OsString::from_vec(b"/saved/cwd\xff".to_vec()).into();
        for current in [
            vec![],
            vec![changed_id],
            vec![changed_kind],
            vec![changed_raw_cwd],
            vec![candidate(RecoveryStatus::Live)],
            vec![candidate(RecoveryStatus::Unavailable)],
        ] {
            let (result, requests) = run(
                vec![Some(response(listing(&current)))],
                &key,
                &CancellationToken::new(),
            );
            let failure = result.unwrap_err();
            assert!(!failure.attempted, "{failure:#}");
            assert!(!transport::outcome_unknown(&failure.error));
            assert_eq!(requests.len(), 1);
            assert!(matches!(requests[0].operation, Operation::RecoveryList));
        }
    }

    #[test]
    fn cancellation_before_submission_does_not_even_list() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let failure = resume(
            &Endpoint::unavailable(),
            &Key::from(&candidate(RecoveryStatus::Recoverable)),
            &cancellation,
        )
        .unwrap_err();
        assert!(!failure.attempted);
        assert!(
            failure
                .error
                .to_string()
                .contains("cancelled before resume submission")
        );
        assert!(!transport::outcome_unknown(&failure.error));
    }

    #[test]
    fn lost_mutation_receipt_is_unknown_and_rejection_is_an_attempt_not_a_start() {
        let source = candidate(RecoveryStatus::Recoverable);
        let key = Key::from(&source);
        let rejection = Response {
            version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some("native conversation became live".into()),
            failure: None,
        };
        for (reply, unknown) in [(None, true), (Some(rejection), false)] {
            let (result, requests) = run(
                vec![
                    Some(response(listing(std::slice::from_ref(&source)))),
                    reply,
                ],
                &key,
                &CancellationToken::new(),
            );
            let failure = result.unwrap_err();
            assert!(failure.attempted);
            assert_eq!(transport::outcome_unknown(&failure.error), unknown);
            assert_eq!(requests.len(), 2);
            assert!(matches!(
                requests[1].operation,
                Operation::PaneResume { .. }
            ));
        }
    }

    #[test]
    fn malformed_success_never_becomes_a_navigation_target() {
        let source = candidate(RecoveryStatus::Recoverable);
        let key = Key::from(&source);
        let mut cases = Vec::new();
        let mut value = started();
        value["source_pane_id"] = json!("foreign-source");
        cases.push(value);
        let mut value = started();
        value["policy"] = json!("saved_native_configuration");
        cases.push(value);
        let mut value = started();
        value["binding"] = json!("loaded");
        cases.push(value);
        let mut value = started();
        value["started"] = json!(false);
        cases.push(value);
        let mut value = started();
        value["workspace_id"] = json!("foreign-workspace");
        cases.push(value);
        let mut value = started();
        value["registry"]["panes"][0]["window_id"] = json!("foreign-window");
        cases.push(value);
        let mut value = started();
        value["registry"]["workspaces"][0]["windows"][0]["layout"] = Value::Null;
        cases.push(value);
        let mut value = started();
        let duplicate = value["registry"]["panes"][0].clone();
        value["registry"]["panes"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        cases.push(value);
        let mut value = started();
        let mut foreign = value["registry"]["workspaces"][0].clone();
        foreign["id"] = json!("other-workspace");
        foreign["windows"][0]["id"] = json!("other-window");
        value["registry"]["workspaces"]
            .as_array_mut()
            .unwrap()
            .push(foreign);
        cases.push(value);
        for value in cases {
            let (result, requests) = run(
                vec![
                    Some(response(listing(std::slice::from_ref(&source)))),
                    Some(response(value)),
                ],
                &key,
                &CancellationToken::new(),
            );
            let failure = result.unwrap_err();
            assert!(failure.attempted);
            assert!(transport::outcome_unknown(&failure.error), "{failure:#}");
            assert_eq!(requests.len(), 2);
        }
    }

    #[test]
    fn unprojected_source_can_start_a_different_exact_target_without_claiming_binding() {
        let source = candidate(RecoveryStatus::Recoverable);
        let key = Key::from(&source);
        let (result, _) = run(
            vec![
                Some(response(listing(&[source]))),
                Some(response(started())),
            ],
            &key,
            &CancellationToken::new(),
        );
        let started = result.unwrap();
        assert_eq!(started.selection.pane, "target");
        assert_eq!(started.source, key);
        assert!(started.registry.panes[0].harness.is_none());
    }

    #[test]
    fn refreshed_live_facts_update_details_and_disable_resume_without_guessing_a_pane() {
        let source = candidate(RecoveryStatus::Recoverable);
        let mut picker = Picker::new(
            Listing::parse(listing(std::slice::from_ref(&source))).unwrap(),
            false,
        );
        picker.list.paste(&source.harness.conversation_id);
        assert!(picker.can_resume());
        let mut live = source.clone();
        live.status = RecoveryStatus::Live;
        live.reason = Some("bound in a different pane".into());
        picker.replace(Listing::parse(listing(&[live])).unwrap());
        assert_eq!(picker.selected().unwrap().pane_id, source.pane_id);
        assert!(!picker.can_resume());
        let details = picker.details();
        assert!(details.iter().any(|row| row.text == "Status: Live"));
        assert!(
            details
                .iter()
                .any(|row| row.text.contains("No live navigation target is implied"))
        );
        assert!(
            details
                .iter()
                .any(|row| row.text == format!("Conversation: {}", source.harness.conversation_id))
        );
        let mut other = source.clone();
        other.harness.conversation_id = "unrelated".into();
        picker.replace(Listing::parse(listing(&[other])).unwrap());
        assert!(
            picker.selected().is_none(),
            "the existing query must survive refresh"
        );
        let readonly = Picker::new(Listing::parse(listing(&[source])).unwrap(), true);
        assert!(readonly.selected().is_some());
        assert!(!readonly.can_resume());
    }
}
