//! Host-owned semantic notification records, scoped policy and resumable queries.
use crate::{HarnessKind, ProcessIdentity, SessionInfo};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Maximum UTF-8 bytes in a notification identity or selector.
pub const MAX_NOTIFICATION_ID_BYTES: usize = 256;
/// Maximum records returned by one notification request.
pub const MAX_NOTIFICATION_LIMIT: u16 = 64;
/// Maximum notification policy rules retained by one host.
pub const MAX_NOTIFICATION_POLICIES: usize = 64;
/// Maximum subscription wait; callers may cancel earlier.
pub const MAX_NOTIFICATION_WAIT_MS: u32 = 5_000;

/// Semantic transition eligible for notification policy; never arbitrary status text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// The harness explicitly needs user action.
    Attention,
    /// The harness explicitly reported failure.
    Error,
    /// Validated working-to-idle transition.
    Idle,
    /// Explicit completion reported by the harness.
    Done,
}

/// Authenticated source retained without credentials, conversation or terminal contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationSource {
    /// Persistent session identity qualified by the observed runtime identities.
    pub session: SessionInfo,
    /// Workspace at observation time, qualified by the session.
    pub workspace_id: Option<String>,
    /// Window at observation time, qualified by the session.
    pub window_id: Option<String>,
    /// Pane at observation time, qualified by the session.
    pub pane_id: String,
    /// Authenticated harness integration.
    pub harness: HarnessKind,
    /// Foreground process identity qualified by its operating-system birth identity.
    pub process: ProcessIdentity,
    /// Managed launch identity when applicable; never a capture capability.
    pub launch_id: Option<String>,
    /// Exact managed native binding generation when applicable.
    pub binding_generation: Option<u64>,
}
impl NotificationSource {
    /// Reject empty, control-containing or over-256-byte identities and invalid process IDs.
    /// Session names may contain up to 1024 UTF-8 bytes and must be valid session names.
    pub fn validate(&self) -> Result<()> {
        crate::valid_session_name(&self.session.name)?;
        ensure!(
            self.session.name.len() <= 1024,
            "notification session name exceeds limit"
        );
        for value in [
            &self.session.session_id,
            &self.session.daemon_id,
            &self.session.session_instance_id,
            &self.pane_id,
            &self.process.birth,
        ] {
            valid_id(value)?;
        }
        for value in [&self.workspace_id, &self.window_id, &self.launch_id]
            .into_iter()
            .flatten()
        {
            valid_id(value)?;
        }
        ensure!(
            self.process.pid != 0,
            "notification process ID must be nonzero"
        );
        Ok(())
    }
}

/// One durable semantic record; its presence does not imply provider delivery or display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    /// Durable, host-store-wide monotonically increasing sequence.
    pub sequence: u64,
    /// Stable record identity, bounded to 256 UTF-8 bytes.
    pub id: String,
    /// Authenticated origin at observation time.
    pub source: NotificationSource,
    /// Allowlisted semantic transition.
    pub kind: NotificationKind,
    /// Optional attention category: permission, question or auth; never arbitrary text.
    pub attention_kind: Option<String>,
    /// Observation time in Unix milliseconds, not a storage-commit receipt.
    pub observed_at_ms: u64,
    /// Retention expiry in Unix milliseconds.
    pub expires_at_ms: u64,
}
impl Notification {
    /// Validate bounded source identities, nonzero sequence, attention category and expiry.
    pub fn validate(&self) -> Result<()> {
        self.source.validate()?;
        valid_id(&self.id)?;
        ensure!(self.sequence > 0, "notification sequence must be nonzero");
        ensure!(
            self.expires_at_ms >= self.observed_at_ms,
            "notification expiry precedes observation"
        );
        if let Some(kind) = &self.attention_kind {
            ensure!(
                self.kind == NotificationKind::Attention,
                "attention category requires attention notification"
            );
            ensure!(
                matches!(kind.as_str(), "permission" | "question" | "auth"),
                "unknown notification attention category"
            );
        }
        Ok(())
    }
}

/// Position in one durable host store, independent of daemon incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationCursor {
    /// Stable store identity, bounded to 256 UTF-8 bytes.
    pub store_id: String,
    /// Last examined sequence, including filtered-out records; zero is a valid baseline.
    pub sequence: u64,
    /// Persisted cumulative dropped-observation count acknowledged by this cursor.
    /// An older value reports a continuity gap once; the store rejects future values.
    pub loss_generation: u64,
}
impl NotificationCursor {
    /// Validate the bounded store identity; the store checks sequence and loss-generation ranges.
    pub fn validate(&self) -> Result<()> {
        valid_id(&self.store_id)
    }
}

/// AND-combined selectors; omitted selectors match every retained source.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationScope {
    /// Persistent session identity, not its name or current daemon incarnation.
    pub session_id: Option<String>,
    /// Workspace selector within the selected sessions.
    pub workspace_id: Option<String>,
    /// Pane selector within the selected sessions.
    pub pane_id: Option<String>,
}
impl NotificationScope {
    /// Reject empty, control-containing or over-256-byte selectors.
    /// Session endpoints must additionally clamp to their own persistent session identity.
    pub fn validate(&self) -> Result<()> {
        for value in [&self.session_id, &self.workspace_id, &self.pane_id]
            .into_iter()
            .flatten()
        {
            valid_id(value)?;
        }
        Ok(())
    }
}

/// Bounded notification filtering shared by inspection and subscriptions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationQuery {
    /// AND-combined source selectors.
    pub scope: NotificationScope,
    /// Allowed kinds; empty means all, with duplicates rejected.
    pub kinds: Vec<NotificationKind>,
    /// Maximum returned records, from 1 through 64 inclusive.
    pub limit: u16,
}
impl Default for NotificationQuery {
    fn default() -> Self {
        Self {
            scope: NotificationScope::default(),
            kinds: Vec::new(),
            limit: MAX_NOTIFICATION_LIMIT,
        }
    }
}
impl NotificationQuery {
    /// Validate bounded selectors, unique kinds and the inclusive 1..64 record limit.
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        valid_kinds(&self.kinds)?;
        ensure!(
            (1..=MAX_NOTIFICATION_LIMIT).contains(&self.limit),
            "notification limit must be 1..64"
        );
        Ok(())
    }
}

/// One scoped enabled-kind override; the most-specific matching rule wins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationPolicy {
    /// Host-wide when empty; workspace/pane rules require an explicit session identity.
    pub scope: NotificationScope,
    /// Enabled kinds; empty disables every kind in this scope.
    pub kinds: Vec<NotificationKind>,
}
impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            scope: NotificationScope::default(),
            kinds: vec![
                NotificationKind::Attention,
                NotificationKind::Error,
                NotificationKind::Idle,
                NotificationKind::Done,
            ],
        }
    }
}
impl NotificationPolicy {
    /// Validate unique kinds and bounded, unambiguous session-qualified policy selectors.
    /// The host enforces the aggregate 64-rule limit and resolves matching specificity.
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        valid_kinds(&self.kinds)?;
        ensure!(
            self.scope.session_id.is_some()
                || (self.scope.workspace_id.is_none() && self.scope.pane_id.is_none()),
            "notification workspace/pane policy requires a session identity"
        );
        Ok(())
    }
}

/// Explicit storage/observation health, not provider delivery status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationHealth {
    /// Whether observations and durable storage are currently healthy.
    pub healthy: bool,
    /// Bounded storage or observation diagnostic when unhealthy.
    pub error: Option<String>,
    /// Number of observations rejected or lost before durable commit.
    pub dropped_observations: u64,
    /// Number of currently retained records.
    pub retained: u64,
}

/// Bounded retained records and resumable progress; no read or delivery acknowledgement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationPage {
    /// Matching records in ascending sequence order, at most the requested limit.
    pub notifications: Vec<Notification>,
    /// Last examined store position, including nonmatching entries.
    pub cursor: NotificationCursor,
    /// Retention or overflow prevents claiming uninterrupted continuity.
    pub gap: bool,
    /// Another retained page is available after the cursor.
    pub has_more: bool,
    /// Storage and observation loss status.
    pub health: NotificationHealth,
}

/// Host-owned records and policy requests; validated before dispatch or queue admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum NotificationRequest {
    /// Inspect retained history; no cursor selects the newest retained page.
    Inspect {
        /// Bounded source, kind and page selectors.
        query: NotificationQuery,
        /// Resume after this cursor; foreign and future cursors are rejected by the store.
        after: Option<NotificationCursor>,
    },
    /// Wait for new records; no cursor establishes a baseline without replaying old alerts.
    Subscribe {
        /// Bounded source, kind and page selectors.
        query: NotificationQuery,
        /// Resume cursor, or absence to start at the current store head.
        after: Option<NotificationCursor>,
        /// Maximum cancellable wait in milliseconds, at most 5000.
        wait_ms: u32,
    },
    /// Read all retained scoped policy rules.
    PolicyGet {},
    /// Replace the rule for one exact scope; accepted only on the host endpoint.
    PolicySet {
        /// Enabled-kind override; at most 64 total rules may be retained.
        policy: NotificationPolicy,
    },
    /// Read observation/storage health even when the retained store cannot be opened.
    /// Does not fabricate a store identity or imply successful durable acceptance.
    Health {},
    /// Manage host-owned encrypted push destinations; never available through session control.
    Push {
        /// Authenticated host management operation.
        request: Box<PushRequest>,
    },
}
impl NotificationRequest {
    /// Reject invalid selectors, cursors, kinds, limits, policy or subscription wait.
    /// Receiving endpoints separately enforce session clamping and host-only writes.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Inspect { query, after } | Self::Subscribe { query, after, .. } => {
                query.validate()?;
                if let Some(after) = after {
                    after.validate()?;
                }
            }
            Self::PolicyGet {} | Self::Health {} => {}
            Self::PolicySet { policy } => policy.validate()?,
            Self::Push { request } => request.validate()?,
        }
        if let Self::Subscribe { wait_ms, .. } = self {
            ensure!(
                *wait_ms <= MAX_NOTIFICATION_WAIT_MS,
                "notification subscription wait exceeds 5000ms"
            );
        }
        Ok(())
    }
    /// Whether this operation requires the explicit authenticated host endpoint.
    pub fn host_only(&self) -> bool {
        matches!(self, Self::PolicySet { .. } | Self::Push { .. })
    }
}

/// Browser-compatible Web Push subscription; every field is confidential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushSubscription {
    /// HTTPS push-service capability URL.
    pub endpoint: String,
    /// Base64url uncompressed P-256 recipient public key.
    pub p256dh: String,
    /// Base64url recipient authentication secret.
    pub auth: String,
}
impl std::fmt::Debug for PushSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PushSubscription([REDACTED])")
    }
}
/// Explicit host-authorized private-provider network exceptions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushEndpointPolicy {
    /// At most eight private-network CIDRs; public HTTPS remains the default.
    pub allow_private_cidrs: Vec<String>,
}
/// Exact destination revision used to fence replacement and revocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushRevision {
    /// Opaque destination identity.
    pub destination_id: String,
    /// Exact destination generation.
    pub generation: u64,
}
/// Public application-server identity; no private signing material.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushPublicKey {
    /// Base64url uncompressed SEC1 public key used when creating a subscription.
    pub public_key: String,
}
/// Secret-free projection of one retained destination generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushDestinationInfo {
    /// Stable logical destination identity.
    pub destination_id: String,
    /// Monotonic subscription/filter generation.
    pub generation: u64,
    /// Pending, active, revoked or expired; never a device delivery status.
    pub state: String,
    /// Pending proof expiry, or the original deadline for a confirmed generation.
    pub challenge_expires_at_ms: u64,
    /// Jobs rejected by bounded admission for this generation.
    pub dropped_jobs: u64,
    /// Human-readable label, retained independently of retired subscription secrets.
    pub device_label: Option<String>,
    /// Source selectors, retained independently of retired subscription secrets.
    pub scope: Option<NotificationScope>,
    /// Selected semantic kinds; an empty list mutes automatic delivery.
    pub kinds: Option<Vec<NotificationKind>>,
    /// Whether automatic event delivery is configured; independent of endpoint expiry.
    #[serde(default)]
    pub enabled: Option<bool>,
}
/// Bounded destination list, prioritizing every active and pending generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushDestinationPage {
    /// At most 128 secret-free generations; older retired generations may be omitted.
    pub destinations: Vec<PushDestinationInfo>,
    /// Maximum retained projections returned by this request.
    pub retained_limit: u16,
}
/// A durable encrypted-challenge admission receipt, not proof of receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushEnrollmentReceipt {
    /// Stable logical destination identity.
    pub destination_id: String,
    /// Exact pending generation required by confirmation.
    pub generation: u64,
    /// Pending proof state at acceptance.
    pub state: String,
    /// Deadline for authenticated confirmation of the encrypted nonce.
    pub challenge_expires_at_ms: u64,
    /// Durable challenge job identity for diagnostic inspection.
    pub job_id: String,
    /// End of the seven-day exact-request deduplication guarantee.
    pub idempotency_expires_at_ms: u64,
}
/// Exact committed destination generation after proof, update or revocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushMutationReceipt {
    /// Stable logical destination identity.
    pub destination_id: String,
    /// Exact committed generation.
    pub generation: u64,
    /// Active or revoked; never a device delivery status.
    pub state: String,
    /// Revocation cannot retract network dispatch already admitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight_outcome: Option<String>,
    /// Filter-update deduplication deadline; proof and revocation are revision-idempotent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_expires_at_ms: Option<u64>,
}
/// Durable test receipt; identical request IDs deduplicate within its explicit horizon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushTestReceipt {
    /// Stable durable job identity.
    pub job_id: String,
    /// Queued at admission; inspect diagnostics for subsequent provider outcomes.
    pub state: String,
    /// End of the seven-day exact-request deduplication guarantee.
    pub idempotency_expires_at_ms: u64,
}
/// Secret-free outcome of one bounded retained push job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushJobDiagnostic {
    /// Durable job identity.
    pub job_id: String,
    /// Stable logical destination identity.
    pub destination_id: String,
    /// Exact subscription generation used by this job.
    pub generation: u64,
    /// Challenge, notification or test.
    pub kind: String,
    /// Durable scheduling/provider outcome, never device delivery.
    pub state: String,
    /// Number of durably admitted network attempts.
    pub attempts: u32,
    /// Stable redacted outcome or absence before the first attempt.
    pub result: Option<String>,
}
/// Bounded push admission and attempt diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushDiagnostics {
    /// At most 128 most-recent job projections.
    pub jobs: Vec<PushJobDiagnostic>,
    /// Cumulative durable job admission losses, independent of diagnostic pruning.
    pub dropped_jobs: u64,
    /// Maximum retained job projections returned by this request.
    pub retained_limit: u16,
    /// Always false: HTTP acceptance never proves device delivery or display.
    pub acceptance_is_delivery: bool,
}

/// Authenticated, host-only push enrollment and diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum PushRequest {
    /// Read the persistent host's public VAPID application-server key.
    PublicKey {},
    /// List secret-free pending and active destinations.
    List {},
    /// Enroll a pending subscription and queue one encrypted nonce challenge.
    Enroll {
        /// Seven-day idempotency key; retries within the receipt horizon require identical arguments.
        request_id: String,
        /// Recipient subscription; never returned by management reads.
        subscription: PushSubscription,
        /// Explicit endpoint network policy.
        policy: PushEndpointPolicy,
        /// Source selectors; no historical events are replayed.
        scope: NotificationScope,
        /// Semantic kinds to deliver.
        kinds: Vec<NotificationKind>,
        /// Active or expired current destination to replace only after successful proof.
        replaces: Option<PushRevision>,
        /// Optional human-readable device label, at most 160 UTF-8 bytes without controls.
        #[serde(default)]
        device_label: Option<String>,
    },
    /// Prove receipt of an encrypted challenge using its private nonce.
    Confirm {
        /// Pending destination identity.
        destination_id: String,
        /// Exact pending generation.
        generation: u64,
        /// Nonce received through the encrypted push channel.
        nonce: String,
    },
    /// Revoke a destination and cancel undispatched work.
    Revoke {
        /// Destination identity.
        destination_id: String,
        /// Exact destination generation.
        generation: u64,
    },
    /// Queue one idempotent encrypted test for an active destination.
    Test {
        /// Seven-day idempotency key; identical retries within the receipt horizon never duplicate sends.
        request_id: String,
        /// Active destination identity.
        destination_id: String,
        /// Exact active generation.
        generation: u64,
    },
    /// Read bounded secret-free provider attempt diagnostics.
    Diagnostics {},
    /// Replace future-event filters without changing the proven subscription.
    Update {
        /// Seven-day idempotency key for this exact filter replacement.
        request_id: String,
        /// Stable logical destination identity.
        destination_id: String,
        /// Exact active subscription generation to replace.
        generation: u64,
        /// Source selectors applied only to newly accepted events.
        scope: NotificationScope,
        /// Semantic kinds to deliver; an empty list disables automatic sends but permits explicit tests.
        kinds: Vec<NotificationKind>,
    },
}
impl std::fmt::Debug for PushRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PushRequest([REDACTED])")
    }
}
impl PushRequest {
    /// Reject unbounded secrets, malformed revisions and ambiguous semantic selectors.
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::PublicKey {} | Self::List {} | Self::Diagnostics {} => {}
            Self::Enroll {
                request_id,
                subscription,
                policy,
                scope,
                kinds,
                replaces,
                device_label,
            } => {
                valid_id(request_id)?;
                ensure!(
                    !subscription.endpoint.is_empty() && subscription.endpoint.len() <= 4096,
                    "push endpoint exceeds bound"
                );
                ensure!(
                    !subscription.p256dh.is_empty() && subscription.p256dh.len() <= 128,
                    "invalid push recipient key bound"
                );
                ensure!(
                    !subscription.auth.is_empty() && subscription.auth.len() <= 128,
                    "invalid push authentication bound"
                );
                ensure!(
                    policy.allow_private_cidrs.len() <= 8
                        && policy
                            .allow_private_cidrs
                            .iter()
                            .all(|cidr| !cidr.is_empty() && cidr.len() <= 64),
                    "push endpoint policy exceeds bound"
                );
                scope.validate()?;
                valid_kinds(kinds)?;
                ensure!(
                    device_label.as_ref().is_none_or(
                        |label| label.len() <= 160 && !label.chars().any(char::is_control)
                    ),
                    "push device label exceeds bound or contains controls"
                );
                if let Some(revision) = replaces {
                    valid_id(&revision.destination_id)?;
                    ensure!(
                        revision.generation > 0 && revision.generation <= i64::MAX as u64,
                        "invalid push generation"
                    );
                }
            }
            Self::Confirm {
                destination_id,
                generation,
                nonce,
            } => {
                valid_id(destination_id)?;
                ensure!(
                    *generation > 0 && *generation <= i64::MAX as u64,
                    "invalid push generation"
                );
                ensure!(
                    !nonce.is_empty() && nonce.len() <= 128,
                    "invalid push challenge bound"
                );
            }
            Self::Update {
                request_id,
                destination_id,
                generation,
                scope,
                kinds,
            } => {
                valid_id(request_id)?;
                valid_id(destination_id)?;
                ensure!(
                    *generation > 0 && *generation < i64::MAX as u64,
                    "invalid push generation"
                );
                scope.validate()?;
                valid_kinds(kinds)?;
            }
            Self::Revoke {
                destination_id,
                generation,
            }
            | Self::Test {
                destination_id,
                generation,
                ..
            } => {
                valid_id(destination_id)?;
                ensure!(
                    *generation > 0 && *generation <= i64::MAX as u64,
                    "invalid push generation"
                );
                if let Self::Test { request_id, .. } = self {
                    valid_id(request_id)?;
                }
            }
        }
        Ok(())
    }
}

fn valid_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= MAX_NOTIFICATION_ID_BYTES
            && !value.chars().any(char::is_control),
        "notification identity must be nonempty, control-free and at most 256 UTF-8 bytes"
    );
    Ok(())
}

fn valid_kinds(kinds: &[NotificationKind]) -> Result<()> {
    ensure!(kinds.len() <= 4, "too many notification kinds");
    for (index, kind) in kinds.iter().enumerate() {
        ensure!(
            !kinds[..index].contains(kind),
            "duplicate notification kind"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_preserve_only_bounded_identity_and_allowlisted_attention_semantics() {
        let mut notification = Notification {
            sequence: 1,
            id: "notification".into(),
            source: NotificationSource {
                session: SessionInfo {
                    name: "session name".into(),
                    session_id: "session".into(),
                    daemon_id: "daemon".into(),
                    session_instance_id: "instance".into(),
                },
                workspace_id: Some("workspace".into()),
                window_id: Some("window".into()),
                pane_id: "pane".into(),
                harness: HarnessKind::Omp,
                process: ProcessIdentity {
                    pid: 1,
                    birth: "birth".into(),
                },
                launch_id: Some("launch".into()),
                binding_generation: Some(0),
            },
            kind: NotificationKind::Attention,
            attention_kind: Some("permission".into()),
            observed_at_ms: 100,
            expires_at_ms: 200,
        };
        assert!(notification.validate().is_ok());
        notification.attention_kind = Some("arbitrary prompt text".into());
        assert!(notification.validate().is_err());
        for kind in ["permission", "question", "auth"] {
            notification.attention_kind = Some(kind.into());
            assert!(notification.validate().is_ok());
        }
        notification.kind = NotificationKind::Error;
        assert!(notification.validate().is_err());
        notification.attention_kind = None;
        assert!(notification.validate().is_ok());
        notification.source.process.pid = 0;
        assert!(notification.validate().is_err());
        notification.source.process.pid = 1;
        notification.source.process.birth = "x".repeat(257);
        assert!(notification.validate().is_err());
        notification.source.process.birth = "birth".into();
        notification.expires_at_ms = 99;
        assert!(notification.validate().is_err());
        notification.expires_at_ms = 200;
        notification.sequence = 0;
        assert!(notification.validate().is_err());
        let mut source = serde_json::to_value(&notification.source).unwrap();
        source["token"] = serde_json::json!("secret");
        assert!(serde_json::from_value::<NotificationSource>(source).is_err());
    }

    #[test]
    fn selectors_and_policy_enforce_byte_bounds_and_qualification() {
        let mut scope = NotificationScope {
            session_id: Some("é".repeat(128)),
            ..Default::default()
        };
        assert!(scope.validate().is_ok());
        scope.session_id.as_mut().unwrap().push('é');
        assert!(scope.validate().is_err());
        for invalid in ["", "bad\nidentity", "bad\0identity"] {
            scope.session_id = Some(invalid.into());
            assert!(scope.validate().is_err());
        }
        let mut policy = NotificationPolicy {
            scope: NotificationScope {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            kinds: Vec::new(),
        };
        assert!(policy.validate().is_err());
        policy.scope.session_id = Some("session".into());
        assert!(policy.validate().is_ok());
        policy.kinds = vec![NotificationKind::Attention; 2];
        assert!(policy.validate().is_err());
    }

    #[test]
    fn request_validation_rejects_limits_waits_and_bad_cursors() {
        for limit in [0, 65, u16::MAX] {
            let query = NotificationQuery {
                limit,
                ..Default::default()
            };
            assert!(
                NotificationRequest::Inspect { query, after: None }
                    .validate()
                    .is_err()
            );
        }
        let subscribe = |wait_ms| NotificationRequest::Subscribe {
            query: NotificationQuery::default(),
            after: None,
            wait_ms,
        };
        assert!(subscribe(0).validate().is_ok());
        assert!(subscribe(5000).validate().is_ok());
        assert!(subscribe(5001).validate().is_err());
        let after = Some(NotificationCursor {
            store_id: String::new(),
            sequence: 0,
            loss_generation: 0,
        });
        assert!(
            NotificationRequest::Inspect {
                query: NotificationQuery::default(),
                after
            }
            .validate()
            .is_err()
        );
        assert!(
            NotificationCursor {
                store_id: "store".into(),
                sequence: 0,
                loss_generation: 0
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn empty_requests_preserve_json_shape_and_reject_unrecognized_fields() {
        for (method, request) in [
            ("policy_get", NotificationRequest::PolicyGet {}),
            ("health", NotificationRequest::Health {}),
        ] {
            let value = serde_json::json!({"method":method});
            assert_eq!(serde_json::to_value(&request).unwrap(), value);
            assert_eq!(
                serde_json::from_value::<NotificationRequest>(value).unwrap(),
                request
            );
            for field in ["prompt", "token", "query"] {
                let mut value = serde_json::json!({"method":method});
                value[field] = serde_json::json!("must not be accepted");
                assert!(serde_json::from_value::<NotificationRequest>(value).is_err());
            }
        }
    }

    #[test]
    fn serde_rejects_arbitrary_semantics_and_unknown_fields() {
        assert!(serde_json::from_str::<NotificationKind>("\"prompt\"").is_err());
        assert!(
            serde_json::from_value::<NotificationRequest>(
                serde_json::json!({"method":"policy_get","prompt":"secret"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<NotificationScope>(serde_json::json!({"token":"secret"}))
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(NotificationKind::Attention).unwrap(),
            "attention"
        );
        assert!(
            serde_json::from_value::<NotificationCursor>(
                serde_json::json!({"store_id":"store","sequence":0})
            )
            .is_err()
        );
        let cursor = NotificationCursor {
            store_id: "store".into(),
            sequence: 7,
            loss_generation: 3,
        };
        let value = serde_json::to_value(&cursor).unwrap();
        assert_eq!(value["loss_generation"], 3);
        assert_eq!(
            serde_json::from_value::<NotificationCursor>(value).unwrap(),
            cursor
        );
    }
}
