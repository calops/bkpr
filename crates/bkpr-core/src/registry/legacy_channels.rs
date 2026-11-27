use super::{Record, Registry};
use crate::channels::ChannelEngine;
use anyhow::Result;
use crossbeam_channel::{Sender, TryRecvError, bounded};
use std::{
    collections::BTreeMap,
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn migrate(
    root: &Path,
    record: &Record,
    channels: &ChannelEngine,
) -> Result<Option<u64>> {
    let directory = root.join("agents");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_dir() => {
            match directory.join("state.json").symlink_metadata() {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {} // Let the guarded legacy opener reject an unsafe directory.
    }
    let workspaces: BTreeMap<_, _> = record
        .registry
        .panes
        .iter()
        .filter_map(|pane| Some((pane.id.as_str(), pane.workspace_id.as_deref()?)))
        .collect();
    channels.maintain_legacy(
        now_ms(),
        |pane| workspaces.get(pane).map(|id| (*id).to_owned()),
        || false,
    )
}

#[derive(Default)]
pub(super) struct Expiry(Mutex<Option<(Sender<()>, JoinHandle<()>)>>);

impl Expiry {
    pub(super) fn start(&self, registry: &Registry, deadline: Option<u64>) -> Result<()> {
        let Some(mut deadline) = deadline else {
            return Ok(());
        };
        let inner = Arc::downgrade(&registry.inner);
        let (stop, stopped) = bounded(0);
        let worker = thread::Builder::new()
            .name("bkpr-channel-expiry".into())
            .spawn(move || {
                let cancelled = || stopped.try_recv() == Err(TryRecvError::Disconnected);
                while stopped.recv_timeout(Duration::from_millis(deadline.saturating_sub(now_ms())))
                    == Err(crossbeam_channel::RecvTimeoutError::Timeout)
                {
                    let Some(inner) = inner.upgrade() else { break };
                    match inner
                        .channels
                        .maintain_legacy(now_ms(), |_| None, cancelled)
                    {
                        Ok(Some(next)) => deadline = next,
                        Ok(None) => break,
                        Err(_) if cancelled() => break,
                        Err(_) => {
                            crate::diagnostics::record("channels.legacy_expiry", None, "deferred");
                            deadline = now_ms().saturating_add(60_000);
                        }
                    }
                }
            })?;
        *self.0.lock().unwrap() = Some((stop, worker));
        Ok(())
    }

    pub(super) fn stop(&self) {
        let worker = self.0.lock().unwrap().take();
        if let Some((stop, worker)) = worker {
            drop(stop);
            let _ = worker.join();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
