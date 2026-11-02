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
            }),
        });
        assert!(!format!("{request:?}").contains("secret-token"));
    }
}
