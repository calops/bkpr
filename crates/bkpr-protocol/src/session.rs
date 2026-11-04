use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const DEFAULT_SESSION_NAME: &str = "bkpr";

/// Names are exact UTF-8 user data, never filesystem path components.
pub fn valid_session_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "session name must be nonempty and contain no control characters"
    );
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    /// Persistent logical identity; independent of any daemon process.
    pub session_id: String,
    /// Fresh daemon incarnation, never restored from storage.
    pub daemon_id: String,
}

/// Immutable daemon startup policy for new panes; never client environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnDefaults {
    pub argv: Vec<String>,
    #[serde(with = "crate::path_wire")]
    pub cwd: std::path::PathBuf,
    #[serde(with = "crate::path_wire")]
    pub home: std::path::PathBuf,
    pub isolated_environment: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ClientAuthority {
    pub client_id: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEvent {
    pub event: String,
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
