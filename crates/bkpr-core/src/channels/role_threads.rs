use super::{ChannelAuthority, ChannelId, Store, WorkspaceParticipant, insert_channel};
use anyhow::Result;
use bkpr_protocol::channels::{
    ChannelSettings, MessageSummary, Participant, ParticipantKind, WakePolicy,
};
use bkpr_protocol::roles::{RoleAttribution, RoleGrant, RoleGrantId};

pub(super) const NAME: &str = "#coordinator";

const SETTINGS: ChannelSettings = ChannelSettings {
    user_wake: WakePolicy::All,
    agent_wake: WakePolicy::None,
    attention_questions: true,
    attention_reports: true,
};

const REPORT_ARCHIVE: &str =
    "UPDATE role_threads SET archive_reported=1 WHERE channel_id=?1 AND archive_reported=0";

impl Store {
    pub(crate) fn role_grant(&self, workspace: &str) -> Option<&RoleGrant> {
        self.roles.get(workspace)
    }

    pub(crate) fn role_grants(&self) -> impl Iterator<Item = &RoleGrant> {
        self.roles.values()
    }

    /// Publish a fresh tenure only after its private channel and catalog entry commit.
    pub(crate) fn set_role(&mut self, grant: RoleGrant) -> Result<Option<RoleGrant>> {
        let tx = self.connection.transaction()?;
        if let Some(previous) = self.roles.get(&grant.workspace_id) {
            tx.execute(REPORT_ARCHIVE, [previous.channel_id.as_ref()])?;
        }
        insert_channel(
            &tx,
            &grant.channel_id,
            &grant.workspace_id,
            &format!("\u{1}{}", grant.channel_id.as_ref()),
            false,
            SETTINGS,
        )?;
        tx.execute(
            "INSERT INTO role_threads(channel_id) VALUES(?1)",
            [grant.channel_id.as_ref()],
        )?;
        tx.commit()?;
        Ok(self.roles.insert(grant.workspace_id.clone(), grant))
    }

    /// Revocation is immediate, even if the archive notice cannot be persisted yet.
    pub(crate) fn remove_role(&mut self, workspace: &str) -> Option<RoleGrant> {
        let former = self.roles.remove(workspace)?;
        self.pending_role_archives.push(former.channel_id.clone());
        Some(former)
    }

    pub(super) fn active_role_grant(&self, workspace: &str) -> Option<&RoleGrant> {
        self.role_grant(workspace)
            .filter(|grant| self.grant_is_live(grant))
    }

    pub(super) fn active_role_thread(
        &self,
        workspace: &str,
        channel: &ChannelId,
    ) -> Option<&RoleGrant> {
        self.role_grant(workspace)
            .filter(|grant| grant.channel_id == *channel && self.grant_is_live(grant))
    }

    fn grant_is_live(&self, grant: &RoleGrant) -> bool {
        self.launch_active
            .as_ref()
            .and_then(|active| active.get())
            .is_some_and(|active| active(&grant.holder.launch_id))
    }

    /// Deliver deferred catalog notices and retire stale wake work. The durable
    /// flag tracks only reporting, never access; the live map is authoritative.
    pub(super) fn report_role_archives(&mut self) -> Result<()> {
        if self.pending_role_archives.is_empty() {
            return Ok(());
        }
        let tx = self.connection.transaction()?;
        {
            let mut report = tx.prepare(REPORT_ARCHIVE)?;
            for channel in &self.pending_role_archives {
                report.execute([channel.as_ref()])?;
            }
        }
        tx.commit()?;
        self.pending_role_archives.clear();
        Ok(())
    }
}

pub(super) fn delegation<'a>(
    grant: Option<&'a RoleGrant>,
    message: &MessageSummary,
) -> Option<&'a RoleGrantId> {
    let role = message.role.as_ref()?;
    let grant = grant?;
    (role.grant_id == grant.id
        && role.role == grant.role
        && message.author.kind == ParticipantKind::Agent)
        .then_some(&grant.id)
}

pub(super) fn attribution(grant: &RoleGrant) -> RoleAttribution {
    RoleAttribution {
        role: grant.role,
        grant_id: grant.id.clone(),
    }
}

pub(super) fn matches_holder(grant: &RoleGrant, actor: &Participant, launch: Option<&str>) -> bool {
    actor.kind == ParticipantKind::Agent && launch == Some(grant.holder.launch_id.as_str())
}

pub(super) fn roster_role(
    grant: Option<&RoleGrant>,
    entry: &WorkspaceParticipant,
) -> Option<RoleAttribution> {
    grant
        .filter(|grant| matches_holder(grant, &entry.actor, entry.launch_id.as_deref()))
        .map(attribution)
}

pub(super) fn constrain_roster(authority: &mut ChannelAuthority, grant: Option<&RoleGrant>) {
    authority.participants.retain(|entry| {
        entry.actor.kind == ParticipantKind::User
            || grant.is_some_and(|grant| {
                matches_holder(grant, &entry.actor, entry.launch_id.as_deref())
            })
    });
}

pub(super) fn reserved_name(name: &str) -> bool {
    let name = name.trim();
    name.strip_prefix('#')
        .unwrap_or(name)
        .eq_ignore_ascii_case("coordinator")
}
