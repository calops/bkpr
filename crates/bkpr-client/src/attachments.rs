use bkpr_protocol::{Size, SpawnDefaults};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// One-shot local bootstrap, admitted only after the physical TUI is running.
/// Cancellation stops waiting, not a daemon or pane already started by it.
pub type PrepareLocal =
    Box<dyn FnOnce(Size, CancellationToken) -> anyhow::Result<()> + Send + 'static>;

/// Independent native session owners. Remote routing is parsed by the launcher;
/// this client never interprets colons or derives remote filesystem defaults.
pub enum SessionTarget {
    /// A local daemon with optional deferred bootstrap.
    Local {
        /// Display/session selection name, if explicitly configured.
        name: Option<String>,
        /// Local daemon control socket.
        socket: PathBuf,
        /// Defaults for newly created panes on this endpoint.
        spawn_defaults: SpawnDefaults,
        /// One-shot bootstrap run off the terminal thread before registration.
        prepare: Option<PrepareLocal>,
    },
    /// An existing session reached through its own SSH transport.
    Remote(crate::remote::RemoteTarget),
}

#[derive(Hash, PartialEq, Eq)]
pub(crate) enum TargetKey {
    Local {
        name: Option<String>,
        socket: PathBuf,
    },
    Remote {
        destination: String,
        session: String,
        helper: Option<PathBuf>,
        ssh_config: Option<PathBuf>,
        forward_agent: bool,
        detach_others: bool,
    },
}

impl SessionTarget {
    pub(crate) fn key(&self) -> TargetKey {
        match self {
            Self::Local { name, socket, .. } => TargetKey::Local {
                name: name.clone(),
                socket: socket.clone(),
            },
            Self::Remote(target) => TargetKey::Remote {
                destination: target.destination.clone(),
                session: target.session_name.clone(),
                helper: target.helper.clone(),
                ssh_config: target.ssh_config.clone(),
                forward_agent: target.forward_agent,
                detach_others: target.detach_others,
            },
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Local { name, socket, .. } => name.clone().unwrap_or_else(|| {
                socket
                    .file_name()
                    .unwrap_or(socket.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            }),
            Self::Remote(target) => {
                format!("\u{f048d} {}:{}", target.destination, target.session_name)
            }
        }
    }

    pub(crate) fn provenance(&self) -> Option<(String, String)> {
        match self {
            Self::Local { .. } => None,
            Self::Remote(target) => Some((target.destination.clone(), target.session_name.clone())),
        }
    }
}
