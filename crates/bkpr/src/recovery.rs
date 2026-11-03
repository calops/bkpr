//! Explicit native recovery through the existing, already-running daemon.
use crate::session::{self, LocalPaths};
use anyhow::{Context, Result, bail, ensure};
use bkpr_client::transport;
use bkpr_protocol::{HarnessRecovery, Operation, RecoveryCandidate, RecoveryStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const NOTICE: &str = "Recoverable means an explicit attempt is allowed; native session, provider and current-profile availability have not been preflighted. Resume uses current native configuration and permissions, not saved argv, model, provider, approval or sandbox settings. Native session state may restore where supported. Provider or conversation mismatch remains a native refusal; Bkpr does not switch or fork. Process start is pending authenticated native conversation binding, not proof the conversation loaded.";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Policy {
    CurrentNativeConfiguration,
}

#[derive(Deserialize, Serialize)]
struct Listing {
    policy: Policy,
    recoveries: Vec<RecoveryCandidate>,
}

fn select(listing: Listing, pane_id: &str) -> Result<HarnessRecovery> {
    ensure!(
        !pane_id.is_empty(),
        "--pane needs an exact nonempty pane ID"
    );
    let mut matches = listing
        .recoveries
        .into_iter()
        .filter(|candidate| candidate.pane_id == pane_id);
    let candidate = matches
        .next()
        .context("no saved recovery matches that exact pane ID in the selected session")?;
    ensure!(
        matches.next().is_none(),
        "daemon returned duplicate recovery pane IDs; refusing ambiguous selection"
    );
    if candidate.status != RecoveryStatus::Recoverable {
        bail!(
            "selected conversation cannot currently resume: {}",
            candidate
                .reason
                .as_deref()
                .unwrap_or(match candidate.status {
                    RecoveryStatus::Live => "conversation is live or resume is already pending",
                    RecoveryStatus::Unavailable => "recovery is unavailable",
                    RecoveryStatus::Recoverable => unreachable!(),
                })
        );
    }
    Ok(candidate.harness)
}

pub fn run(paths: &LocalPaths, pane_id: Option<&str>) -> Result<Value> {
    session::validate_target(paths).map_err(|error| {
        let absent = error.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
        });
        if absent {
            error.context("selected daemon is not running; first open normal bkpr with the same session target to restart the backend, then retry resume; no daemon was started")
        } else {
            error
        }
    })?;
    let listing: Listing =
        serde_json::from_value(transport::checked(&paths.socket, Operation::RecoveryList)?)
            .context("decoding current-native-configuration recovery list")?;
    let Some(pane_id) = pane_id else {
        return Ok(json!({
            "policy": listing.policy,
            "notice": NOTICE,
            "recoveries": listing.recoveries,
        }));
    };
    let expected = select(listing, pane_id)?;
    eprintln!("{NOTICE}");
    // The daemon rechecks this exact descriptor and reserves the native identity.
    // A disconnected mutation is never retried by this launcher.
    let mut result = transport::checked(
        &paths.socket,
        Operation::PaneResume {
            pane_id: pane_id.to_owned(),
            expected,
        },
    )?;
    let object = result
        .as_object_mut()
        .context("daemon omitted structured resume result; do not replay")?;
    object.insert("policy".into(), json!(Policy::CurrentNativeConfiguration));
    object.insert("notice".into(), json!(NOTICE));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bkpr_protocol::HarnessKind;
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

    fn candidate(pane_id: &str, status: RecoveryStatus) -> RecoveryCandidate {
        RecoveryCandidate {
            pane_id: pane_id.into(),
            workspace_id: Some("workspace".into()),
            window_id: Some("window".into()),
            window_revision: Some(3),
            title: "not-an-id".into(),
            harness: HarnessRecovery {
                kind: HarnessKind::Omp,
                conversation_id: "native-conversation".into(),
                cwd: PathBuf::from(OsString::from_vec(b"/native/nonutf8-\xff".to_vec())),
            },
            status,
            reason: None,
        }
    }

    fn listing(recoveries: Vec<RecoveryCandidate>) -> Listing {
        Listing {
            policy: Policy::CurrentNativeConfiguration,
            recoveries,
        }
    }

    #[test]
    fn exact_selection_preserves_native_binding_and_raw_cwd() {
        let target = candidate("pane-123", RecoveryStatus::Recoverable);
        let expected = target.harness.clone();
        let selected = select(
            listing(vec![
                candidate("pane-1234", RecoveryStatus::Recoverable),
                target,
            ]),
            "pane-123",
        )
        .unwrap();
        assert_eq!(selected, expected);
        for id in [
            "",
            "pane-12",
            "not-an-id",
            "native-conversation",
            "foreign-pane",
        ] {
            assert!(
                select(
                    listing(vec![candidate("pane-123", RecoveryStatus::Recoverable)]),
                    id
                )
                .is_err()
            );
        }
    }

    #[test]
    fn selection_refuses_live_unavailable_and_ambiguous_receipts() {
        for status in [RecoveryStatus::Live, RecoveryStatus::Unavailable] {
            assert!(select(listing(vec![candidate("pane", status)]), "pane").is_err());
        }
        assert!(
            select(
                listing(vec![
                    candidate("pane", RecoveryStatus::Recoverable),
                    candidate("pane", RecoveryStatus::Recoverable),
                ]),
                "pane",
            )
            .is_err()
        );
    }

    #[test]
    fn unsupported_recovery_policy_is_not_silently_accepted() {
        assert!(
            serde_json::from_value::<Listing>(json!({
                "policy": "replay_saved_settings",
                "recoveries": [],
            }))
            .is_err()
        );
    }
}
