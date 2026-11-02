use super::*;
use std::{
    os::unix::net::UnixStream,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Serialize, Deserialize)]
struct SessionRecord {
    version: u32,
    name: String,
    session_id: String,
}

pub(super) fn open_session(root: &Path, name: &str) -> Result<SessionInfo> {
    let stored = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("session.json"))
    {
        Ok(mut file) => {
            check_file(&file)?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(MAX_RECORD + 1)
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= MAX_RECORD, "session metadata limit");
            let record: SessionRecord =
                serde_json::from_slice(&bytes).context("decode session metadata")?;
            ensure!(
                record.version == 1 && Uuid::parse_str(&record.session_id).is_ok(),
                "invalid session identity"
            );
            valid_session_name(&record.name)?;
            ensure!(
                record.name == name,
                "state root belongs to a different session name"
            );
            record
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let record = SessionRecord {
                version: 1,
                name: name.to_owned(),
                session_id: Uuid::new_v4().to_string(),
            };
            let mut temporary = tempfile::NamedTempFile::new_in(root)?;
            serde_json::to_writer(temporary.as_file_mut(), &record)?;
            temporary.as_file_mut().flush()?;
            ensure!(
                temporary.as_file().metadata()?.len() <= MAX_RECORD,
                "session metadata limit"
            );
            temporary.as_file().sync_all()?;
            temporary.persist(root.join("session.json"))?;
            File::open(root)?.sync_all()?;
            record
        }
        Err(error) => return Err(error.into()),
    };
    Ok(SessionInfo {
        name: stored.name,
        session_id: stored.session_id,
        daemon_id: Uuid::new_v4().to_string(),
    })
}

/// A managed view and its queued native work retain this revocable capability.
/// No Debug/Serialize: the token and process observer are strictly ephemeral.
pub(crate) struct ClientLease {
    pub(crate) id: String,
    token: String,
    owner: crate::transport::ClientOwner,
    lifetime: UnixStream,
    active: AtomicBool,
    activated: AtomicBool,
    detached: AtomicBool,
    handshake: AtomicBool,
}
impl ClientLease {
    pub(crate) fn ensure_active(&self) -> Result<()> {
        ensure!(
            self.is_active(),
            "client attachment is inactive or detached"
        );
        Ok(())
    }
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) && !self.detached.load(Ordering::Acquire)
    }
    pub(crate) fn owner(&self) -> &crate::transport::ClientOwner {
        &self.owner
    }
    pub(crate) fn detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }
    pub(crate) fn handshake_complete(&self) {
        self.handshake.store(true, Ordering::Release);
    }
    fn invalidate(&self) {
        self.detached.store(true, Ordering::Release);
        self.active.store(false, Ordering::Release);
    }
    fn notify(&self, reason: &str) {
        let event = ClientEvent {
            event: "client_detached".into(),
            reason: reason.into(),
        };
        if self.handshake.load(Ordering::Acquire)
            && let Ok(mut bytes) = serde_json::to_vec(&event)
        {
            bytes.push(b'\n');
            crate::transport::send_lifetime_event(&self.lifetime, &bytes);
        }
        let _ = self.lifetime.shutdown(std::net::Shutdown::Both);
    }
}

impl Registry {
    pub(crate) fn register_client(
        &self,
        stream: &UnixStream,
        session_id: &str,
        daemon_id: &str,
    ) -> Result<(Arc<ClientLease>, Value)> {
        ensure!(
            session_id == self.inner.session.session_id
                && daemon_id == self.inner.session.daemon_id,
            "session or daemon identity changed"
        );
        let owner = crate::transport::ClientOwner::authenticate(stream)?;
        let client = Arc::new(ClientLease {
            id: Uuid::new_v4().to_string(),
            token: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            owner,
            lifetime: stream.try_clone()?,
            active: AtomicBool::new(false),
            activated: AtomicBool::new(false),
            detached: AtomicBool::new(false),
            handshake: AtomicBool::new(false),
        });
        let mut clients = self.inner.clients.lock().unwrap();
        ensure!(
            !self.inner.state.lock().unwrap().closing,
            "daemon is shutting down"
        );
        ensure!(clients.len() < MAX_CLIENTS, "client attachment limit");
        clients.insert(client.id.clone(), client.clone());
        let response = json!({"client":ClientAuthority {client_id:client.id.clone(),token:client.token.clone()},"session":self.inner.session});
        Ok((client, response))
    }
    pub(crate) fn client_request(
        &self,
        authority: ClientAuthority,
        operation: Operation,
        stream: &UnixStream,
    ) -> Result<Value> {
        let client = {
            let clients = self.inner.clients.lock().unwrap();
            let client = clients
                .get(&authority.client_id)
                .context("unknown or detached client")?;
            ensure!(
                crate::harness::constant_equal(client.token.as_bytes(), authority.token.as_bytes())
                    && !client.detached(),
                "invalid client authority"
            );
            client.owner.validate(stream)?;
            client.clone()
        };
        match operation {
            Operation::ClientActivate { detach_others } => {
                self.activate_client(&client, detach_others)
            }
            Operation::ClientDetach => {
                self.detach_client(&client, "detached");
                Ok(json!({"detached":true}))
            }
            Operation::Client { .. }
            | Operation::ClientRegister { .. }
            | Operation::Shutdown
            | Operation::HarnessBegin { .. }
            | Operation::HarnessEvent { .. }
            | Operation::HarnessContext { .. }
            | Operation::Agent { .. } => {
                bail!("operation is not available through a managed client")
            }
            operation => self.request_for_client(Request::new(operation), Some(client)),
        }
    }
    fn activate_client(&self, client: &Arc<ClientLease>, detach_others: bool) -> Result<Value> {
        let retired = {
            let mut clients = self.inner.clients.lock().unwrap();
            ensure!(
                !self.inner.state.lock().unwrap().closing,
                "daemon is shutting down"
            );
            ensure!(
                clients
                    .get(&client.id)
                    .is_some_and(|current| Arc::ptr_eq(current, client))
                    && !client.detached()
                    && client.owner.is_alive(),
                "client attachment ended before activation"
            );
            ensure!(
                crate::transport::lifetime_connected(&client.lifetime),
                "client lifetime stream disconnected before activation"
            );
            if client.activated.swap(true, Ordering::AcqRel) {
                return Ok(json!({"activated":true,"already_active":true}));
            }
            client.active.store(true, Ordering::Release);
            if detach_others {
                let retired = clients
                    .values()
                    .filter(|other| !Arc::ptr_eq(other, client))
                    .cloned()
                    .collect::<Vec<_>>();
                for other in &retired {
                    other.invalidate();
                    clients.remove(&other.id);
                }
                retired
            } else {
                Vec::new()
            }
        };
        for other in retired {
            self.finish_client_detach(&other, "takeover");
        }
        Ok(json!({"activated":true,"already_active":false}))
    }
    pub(crate) fn detach_client(&self, client: &Arc<ClientLease>, reason: &str) {
        let removed = {
            let mut clients = self.inner.clients.lock().unwrap();
            if !clients
                .get(&client.id)
                .is_some_and(|current| Arc::ptr_eq(current, client))
            {
                return;
            }
            client.invalidate();
            clients.remove(&client.id)
        };
        if removed.is_some() {
            self.finish_client_detach(client, reason);
        }
    }
    pub(super) fn detach_all_clients(&self, reason: &str) {
        let clients = {
            let mut clients = self.inner.clients.lock().unwrap();
            for client in clients.values() {
                client.invalidate();
            }
            std::mem::take(&mut *clients)
        };
        for client in clients.values() {
            self.finish_client_detach(client, reason);
        }
    }
    fn finish_client_detach(&self, client: &Arc<ClientLease>, reason: &str) {
        client.notify(reason);
        let panes = self
            .inner
            .state
            .lock()
            .unwrap()
            .live
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pane in panes {
            pane.detach_client_views(client);
        }
    }
}

#[cfg(test)]
mod tests;
