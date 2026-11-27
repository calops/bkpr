use super::{App, Background, jobs};
use crate::transport::{self, Endpoint};
use anyhow::{Result, anyhow};
use bkpr_protocol::roles::{RoleApproval, RoleRequest, RoleRequestId, RoleResponse, WorkspaceRole};
use bkpr_protocol::{Operation, Payload, Size};
use bkpr_tui_framework::{Rect, Role, RoleSpan, Theme, UiSurface, safe_text, text_width, window};

pub(super) const ACTIVITY: &str = "roles:activity";
pub(super) const SNAPSHOT: &str = "roles:snapshot";
pub(super) const DECISION: &str = "roles:decision";

pub(super) fn request(endpoint: &Endpoint, request: RoleRequest) -> Result<RoleResponse> {
    let mutation = matches!(request, RoleRequest::Decide { .. } | RoleRequest::Set { .. });
    match endpoint.checked_payload(Operation::Roles { request })? {
        Payload::Roles(response) => Ok(response),
        _ => {
            let error = anyhow!("session returned a non-role response");
            Err(if mutation { transport::mark_outcome_unknown(error) } else { error })
        }
    }
}

pub(super) fn label(role: WorkspaceRole) -> &'static str {
    match role {
        WorkspaceRole::Coordinator => "Coordinator",
    }
}

#[derive(Default)]
pub(super) struct State {
    supported: Option<bool>,
    eligible: Option<bool>,
    reported: Option<bool>,
    sending_activity: Option<bool>,
    activity_pending: bool,
    snapshot_pending: bool,
    revision: u64,
    approvals: Vec<RoleApproval>,
    focus_epoch: u64,
    snapshot_epoch: u64,
    surface_open: bool,
    presented: bool,
    deciding: Option<RoleRequestId>,
    spent: Vec<RoleRequestId>,
}

impl State {
    pub(super) fn supported(&self) -> bool {
        self.supported == Some(true)
    }

    pub(super) fn eligible(&self) -> bool {
        self.eligible == Some(true)
    }

    pub(super) fn observe(&mut self, eligible: bool, interaction: bool) {
        if self.supported == Some(false) {
            return;
        }
        let changed = self.eligible != Some(eligible);
        if changed {
            self.focus_epoch = self.focus_epoch.wrapping_add(1);
            self.snapshot_pending |= eligible;
        }
        if changed || eligible && interaction {
            self.eligible = Some(eligible);
            self.activity_pending = true;
        }
        if !eligible {
            self.clear_prompt();
        }
    }

    pub(super) fn next_activity(&mut self) -> Option<RoleRequest> {
        if !self.activity_pending || self.sending_activity.is_some() {
            return None;
        }
        let eligible = self.eligible();
        self.activity_pending = false;
        self.sending_activity = Some(eligible);
        Some(RoleRequest::Activity { eligible })
    }

    pub(super) fn complete_activity(&mut self, supported: bool) {
        self.supported = Some(supported);
        if supported {
            if self.sending_activity == Some(true) && self.reported != Some(true) {
                self.snapshot_pending = true;
            }
            self.reported = self.sending_activity.take();
        } else {
            self.sending_activity = None;
            self.activity_pending = false;
            self.clear_prompt();
        }
    }

    fn clear_prompt(&mut self) {
        self.approvals.clear();
        self.presented = false;
    }

    pub(super) fn invalidate(&mut self, revision: u64) {
        if revision > self.revision {
            self.revision = revision;
            self.clear_prompt();
            self.snapshot_pending = true;
        }
    }

    pub(super) fn next_snapshot(&mut self) -> Option<RoleRequest> {
        if self.snapshot_pending
            && self.supported()
            && self.reported == Some(true)
            && self.sending_activity != Some(false)
            && self.eligible()
        {
            self.snapshot_epoch = self.focus_epoch;
            self.snapshot_pending = false;
            Some(RoleRequest::Snapshot {})
        } else {
            None
        }
    }

    pub(super) fn apply_snapshot(&mut self, revision: u64, mut approvals: Vec<RoleApproval>) {
        if revision < self.revision
            || self.snapshot_epoch != self.focus_epoch
            || !self.eligible()
            || self.reported != Some(true)
        {
            return;
        }
        self.revision = revision;
        self.spent.retain(|id| approvals.iter().any(|approval| &approval.id == id));
        approvals.retain(|approval| !self.spent.contains(&approval.id));
        self.presented &= self.approvals.first() == approvals.first();
        self.approvals = approvals;
        self.snapshot_pending = false;
    }

    pub(super) fn prompt(&self) -> Option<&RoleApproval> {
        if self.eligible() && self.deciding.is_none() {
            self.approvals.first()
        } else {
            None
        }
    }

    pub(super) fn mark_presented(&mut self, present: bool) {
        self.presented = present;
    }

    pub(super) fn decision(&self, approve: bool) -> Option<RoleRequest> {
        let prompt = self.prompt().filter(|_| self.presented)?;
        Some(RoleRequest::Decide {
            request_id: prompt.id.clone(),
            requester: prompt.requester.clone(),
            expected_holder: prompt.prior_holder.as_ref().map(|holder| holder.id.clone()),
            approve,
        })
    }

    pub(super) fn decision_started(&mut self) {
        self.deciding = self.approvals.first().map(|approval| approval.id.clone());
        self.presented = false;
    }

    pub(super) fn complete_decision(&mut self) {
        if let Some(id) = self.deciding.take() {
            self.spent.push(id);
        }
        self.clear_prompt();
        self.snapshot_pending = true;
    }
}

impl App {
    pub(super) fn role_activity(&mut self, physical_focus: Option<bool>, interaction: bool) {
        let eligible = !self.options.read_only
            && !self.backgrounded
            && physical_focus.unwrap_or(self.roles.eligible() || interaction);
        self.roles.observe(eligible, interaction);
    }

    pub(super) fn clipboard_prompt_open(&self) -> bool {
        self.consent
            .as_ref()
            .is_some_and(|consent| !consent.state.allowed && !consent.state.sent)
    }

    pub(super) fn role_prompt(&self) -> Option<&RoleApproval> {
        if self.clipboard_prompt_open()
            || self.image_viewer.is_some()
            || self.diagnostic_active()
            || self.sidebar_menu.is_some()
        {
            return None;
        }
        self.roles.prompt()
    }

    pub(super) fn consent_prompt_open(&self) -> bool {
        self.clipboard_prompt_open() || self.role_prompt().is_some()
    }

    pub(super) fn sync_role_surface(&mut self) {
        let open = self.role_prompt().is_some();
        if self.roles.surface_open == open {
            return;
        }
        self.roles.surface_open = open;
        self.roles.mark_presented(false);
        if let Some(button) = self.overlays.take_pointer_capture() {
            self.retired_overlay_button = Some(button);
        }
        if open {
            self.retire_resize();
            self.selection_focus_lost();
            self.chat.cancel_editor_press();
            self.recovery.cancel_button_press();
            if let Some(finder) = &mut self.finder {
                finder.cancel_copy_press();
            }
        }
    }

    fn submit_role_job(&mut self, key: &'static str, request: RoleRequest) -> Result<()> {
        let endpoint = self.socket.clone();
        self.jobs.submit(key.into(), move || {
            Ok(Background::Role(Box::new(self::request(&endpoint, request))))
        })
    }

    pub(super) fn schedule_roles(&mut self) {
        if self.jobs.is_full() {
            return;
        }
        if !self.jobs.contains(SNAPSHOT)
            && let Some(request) = self.roles.next_snapshot()
        {
            if let Err(error) = self.submit_role_job(SNAPSHOT, request) {
                self.report_failure("Couldn't load workspace role requests.", &error);
            }
        } else if let Some(request) = self.roles.next_activity() {
            if let Err(error) = self.submit_role_job(ACTIVITY, request) {
                self.roles.complete_activity(false);
                self.chat.set_roles_enabled(false);
                self.report_failure("Couldn't register for workspace roles.", &error);
            }
        }
    }

    pub(super) fn decide_role(&mut self, approve: bool) {
        if let Some(request) = self.roles.decision(approve) {
            match self.submit_role_job(DECISION, request) {
                Ok(()) => self.roles.decision_started(),
                Err(error) => {
                    self.report_failure("Role decision wasn't sent.", &error);
                }
            }
        }
    }

    pub(super) fn complete_roles(&mut self, completion: jobs::Completion<Background>) {
        let result = role_receipt(completion.result);
        match completion.key.as_str() {
            ACTIVITY => {
                let supported = matches!(result, Ok(RoleResponse::Activity {}));
                self.roles.complete_activity(supported);
                self.chat.set_roles_enabled(supported);
            }
            SNAPSHOT => match result {
                Ok(RoleResponse::Snapshot { revision, approvals, .. }) => {
                    self.roles.apply_snapshot(revision, approvals);
                }
                result => {
                    let error = result.err().unwrap_or_else(|| anyhow!("unexpected role snapshot"));
                    self.report_failure("Couldn't load workspace role requests.", &error);
                }
            },
            DECISION => {
                self.roles.complete_decision();
                if let Err(error) = decision_receipt(result) {
                    self.report_failure("Couldn't confirm the workspace role decision.", &error);
                }
            }
            _ => unreachable!("unknown workspace role job"),
        }
    }
}

fn role_receipt(result: Result<Background>) -> Result<RoleResponse> {
    result.and_then(|background| match background {
        Background::Role(result) => *result,
        _ => Err(anyhow!("role job returned an unrelated completion")),
    })
}

fn decision_receipt(result: Result<RoleResponse>) -> Result<()> {
    match result? {
        RoleResponse::Updated { .. } => Ok(()),
        _ => Err(transport::mark_outcome_unknown(anyhow!(
            "unexpected workspace role decision receipt"
        ))),
    }
}

pub(super) fn finish(completion: jobs::Completion<Background>) -> Result<()> {
    if completion.key == DECISION {
        decision_receipt(role_receipt(completion.result)).map_err(|error| {
            error.context("couldn't confirm the workspace role decision; do not replay")
        })
    } else {
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Paint {
    source: Option<(RoleApproval, Size, u64, bool)>,
}

impl Paint {
    pub(super) fn clear(&mut self) {
        self.source = None;
    }

    pub(super) fn paint(
        &mut self,
        surface: &mut UiSurface,
        size: Size,
        theme: &Theme,
        prompt: &RoleApproval,
    ) -> bool {
        if let Some((previous, old_size, old_theme, complete)) = &self.source
            && previous == prompt
            && *old_size == size
            && *old_theme == theme.revision()
        {
            return *complete;
        }
        let complete = paint(surface, size, theme, prompt);
        self.source = Some((prompt.clone(), size, theme.revision(), complete));
        complete
    }
}

fn paint(surface: &mut UiSurface, size: Size, theme: &Theme, prompt: &RoleApproval) -> bool {
    let role = label(prompt.role);
    let mut lines = vec![format!("{role} request")];
    lines.push(identity("Requester: ", &prompt.participant.display_name, &prompt.requester.launch_id, size.cols));
    lines.push(format!("Workspace: {}", safe_text(&prompt.workspace_name)));
    if let Some(holder) = &prompt.prior_holder {
        lines.push(identity("Replace: ", &holder.display_name, &holder.holder.launch_id, size.cols));
    }
    let complete = usize::from(size.rows) >= lines.len() && size.cols >= 20;
    let title = line(0, size.cols);
    surface.paint(
        size,
        lines.into_iter().take(usize::from(size.rows)).enumerate()
            .map(|(y, text)| (line(y as u16, size.cols), window::fit(&text, size.cols), false))
            .collect(),
        None,
    );
    surface.apply_theme(theme, Role::Panel, &[RoleSpan { rect: title, role: Role::Warning }]);
    complete
}

fn line(y: u16, width: u16) -> Rect {
    Rect { x: 0, y, width, height: 1 }
}

fn identity(prefix: &str, name: &str, launch: &str, width: u16) -> String {
    let suffix: String = launch.chars().take(8).collect();
    let budget = usize::from(width).saturating_sub(text_width(prefix) + text_width(&suffix) + 3);
    let name = window::fit(&safe_text(name), budget as u16);
    format!("{prefix}{name} · {}", safe_text(&suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::channels::{ChannelId, ChannelMentionBinding, Participant, ParticipantId, ParticipantKind};
    use bkpr_protocol::roles::{RoleGrant, RoleGrantId, RoleOrigin};

    fn prompt() -> RoleApproval {
        let binding = |id: &str| ChannelMentionBinding {
            participant_id: ParticipantId(id.into()),
            launch_id: id.into(),
            binding_generation: 1,
        };
        RoleApproval {
            id: RoleRequestId("request".into()),
            workspace_id: "workspace".into(),
            role: WorkspaceRole::Coordinator,
            requester: binding("requester"),
            participant: Participant {
                id: ParticipantId("requester".into()),
                kind: ParticipantKind::Agent,
                display_name: "Twin".into(),
            },
            prior_holder: Some(RoleGrant {
                id: RoleGrantId("previous".into()),
                workspace_id: "workspace".into(),
                role: WorkspaceRole::Coordinator,
                holder: binding("holder"),
                display_name: "Twin".into(),
                origin: RoleOrigin::Command,
                granted_at_ms: 1,
                channel_id: ChannelId("old-thread".into()),
            }),
            workspace_name: "Workspace".into(),
            expires_at_ms: 2,
        }
    }

    #[test]
    fn withdrawn_prompt_cannot_act_and_late_snapshot_cannot_restore_it() {
        let mut state = State::default();
        state.observe(true, false);
        assert!(matches!(state.next_activity(), Some(RoleRequest::Activity { eligible: true })));
        state.complete_activity(true);
        state.next_snapshot().unwrap();
        let approval = prompt();
        state.apply_snapshot(1, vec![approval.clone()]);
        assert!(state.decision(true).is_none());
        state.mark_presented(true);
        assert_eq!(
            state.decision(true),
            Some(RoleRequest::Decide {
                request_id: approval.id.clone(),
                requester: approval.requester.clone(),
                expected_holder: Some(approval.prior_holder.as_ref().unwrap().id.clone()),
                approve: true,
            }),
        );
        state.invalidate(2);
        state.apply_snapshot(1, vec![approval]);
        assert!(state.prompt().is_none());
        assert!(state.decision(false).is_none());
    }

    #[test]
    fn focus_change_retires_a_snapshot_in_flight() {
        let mut state = State::default();
        state.observe(true, true);
        state.next_activity().unwrap();
        state.complete_activity(true);
        state.next_snapshot().unwrap();
        state.observe(false, false);
        state.observe(true, true);
        state.apply_snapshot(1, vec![prompt()]);
        assert!(state.prompt().is_none());
        state.next_snapshot().unwrap();
        state.apply_snapshot(1, vec![prompt()]);
        assert!(state.prompt().is_some());
    }
}
