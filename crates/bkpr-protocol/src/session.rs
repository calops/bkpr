//! Logical session identity, daemon startup defaults and attached-client authority.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Session name selected when the caller supplies no explicit name.
pub const DEFAULT_SESSION_NAME: &str = "bkpr";

/// Validate an exact UTF-8 session name without interpreting it as a path.
///
/// Returns an error for an empty name or any control character.
pub fn valid_session_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "session name must be nonempty and contain no control characters"
    );
    Ok(())
}

/// Persistent session identity qualified by the daemon incarnation serving it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Exact user-visible session name, not a filesystem path component.
    pub name: String,
    /// Persistent logical identity; independent of any daemon process.
    pub session_id: String,
    /// Fresh daemon incarnation, never restored from storage.
    pub daemon_id: String,
}

/// Immutable daemon startup policy for new panes; never client environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnDefaults {
    /// Default program and arguments used when a new pane supplies no command.
    pub argv: Vec<String>,
    /// Default absolute launch directory, preserving native path bytes.
    #[serde(with = "crate::path_wire")]
    pub cwd: std::path::PathBuf,
    /// Absolute HOME directory used for newly spawned pane processes.
    #[serde(with = "crate::path_wire")]
    pub home: std::path::PathBuf,
    /// Clear inherited environment and use the isolated launch environment.
    pub isolated_environment: bool,
}

/// Secret authority for one attached client; debug output redacts the token.
#[derive(Clone, Serialize, Deserialize)]
pub struct ClientAuthority {
    /// Opaque identity assigned to the attached client.
    pub client_id: String,
    /// Secret used to authenticate client-scoped operations.
    pub token: String,
}
impl std::fmt::Debug for ClientAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientAuthority")
            .field("client_id", &self.client_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// Daemon lifecycle notification on an attached client's event stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEvent {
    /// Event discriminator, currently `client_detached` for forced detachment.
    pub event: String,
    /// Human-facing explanation of the lifecycle transition.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_exact_and_not_path_sanitized() {
        for name in ["bkpr", " ", "../other", ".", "..", "a/b", "日本語 🐈"] {
            valid_session_name(name).unwrap();
        }
        for name in ["", "x\n", "\0", "\u{7f}"] {
            assert!(valid_session_name(name).is_err());
        }
    }

    #[test]
    fn credentials_are_redacted_even_in_nested_operation_debug() {
        let authority = ClientAuthority {
            client_id: "client".into(),
            token: "secret-token".into(),
        };
        assert!(!format!("{authority:?}").contains("secret-token"));
        let request = crate::Request::new(crate::Operation::Client {
            authority,
            request: Box::new(crate::Operation::ClientActivate {
                detach_others: true,
                agent_socket: Some("/private/source-agent.sock".into()),
            }),
        });
        assert!(!format!("{request:?}").contains("secret-token"));
        assert!(!format!("{request:?}").contains("/private/source-agent.sock"));
    }

    #[test]
    fn activation_preserves_explicit_absence_without_an_ambient_default() {
        let request = crate::Request::new(crate::Operation::ClientActivate {
            detach_others: false,
            agent_socket: None,
        });
        let value = serde_json::to_value(request).unwrap();
        assert!(value["agent_socket"].is_null());
        let decoded: crate::Request = serde_json::from_value(value).unwrap();
        assert!(matches!(
            decoded.operation,
            crate::Operation::ClientActivate {
                agent_socket: None,
                ..
            }
        ));
        assert!(
            serde_json::from_value::<crate::Request>(serde_json::json!({
                "version": crate::PROTOCOL_VERSION,
                "op": "client_activate",
                "detach_others": false,
            }))
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn activation_source_preserves_raw_unix_bytes_and_stays_out_of_debug() {
        use std::os::unix::ffi::OsStringExt;
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"/private/source-\xff.sock".to_vec(),
        ));
        let request = crate::Request::new(crate::Operation::ClientActivate {
            detach_others: false,
            agent_socket: Some(path.clone()),
        });
        let value = serde_json::to_value(&request).unwrap();
        assert!(value["agent_socket"]["unix_bytes"].is_array());
        let decoded: crate::Request = serde_json::from_value(value).unwrap();
        let crate::Operation::ClientActivate { agent_socket, .. } = decoded.operation else {
            panic!("expected activation");
        };
        assert_eq!(agent_socket, Some(path));
        let debug = format!("{request:?}");
        assert!(!debug.contains("source-"));
        assert!(!debug.contains("unix_bytes"));
    }
}
