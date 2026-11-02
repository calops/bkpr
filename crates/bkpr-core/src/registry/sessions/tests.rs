use super::*;
use std::{
    io::{BufRead, BufReader},
    thread,
    time::Instant,
};

fn wire(socket: &Path, operation: Operation) -> Value {
    wire_request(socket, Request::new(operation))
}
fn wire_request(socket: &Path, request: Request) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    serde_json::to_writer(&mut stream, &request).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "client lifecycle did not converge"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
struct Server {
    registry: Registry,
    socket: PathBuf,
    worker: Option<thread::JoinHandle<Result<()>>>,
    _root: tempfile::TempDir,
}
impl Server {
    fn new(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open_named(&root.path().join("state"), name).unwrap();
        let socket = root.path().join("daemon.sock");
        let listener = crate::transport::bind(&socket).unwrap();
        let serving = registry.clone();
        let worker = thread::spawn(move || listener.serve(serving));
        Self {
            registry,
            socket,
            worker: Some(worker),
            _root: root,
        }
    }
    fn session(&self) -> SessionInfo {
        serde_json::from_value(
            wire(&self.socket, Operation::SessionInfo)["result"]["session"].clone(),
        )
        .unwrap()
    }
    fn register(&self) -> Client {
        let session = self.session();
        let mut stream = UnixStream::connect(&self.socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        serde_json::to_writer(
            &mut stream,
            &Request::new(Operation::ClientRegister {
                session_id: session.session_id,
                daemon_id: session.daemon_id,
            }),
        )
        .unwrap();
        stream.write_all(b"\n").unwrap();
        let mut stream = BufReader::new(stream);
        let mut bytes = Vec::new();
        stream.read_until(b'\n', &mut bytes).unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["ok"], true, "{response}");
        let authority = serde_json::from_value(response["result"]["client"].clone()).unwrap();
        Client { stream, authority }
    }
    fn managed(&self, client: &Client, request: Operation) -> Value {
        wire(
            &self.socket,
            Operation::Client {
                authority: client.authority.clone(),
                request: Box::new(request),
            },
        )
    }
    fn activate(&self, client: &Client, detach_others: bool) -> Value {
        self.managed(
            client,
            Operation::ClientActivate {
                detach_others,
                agent_socket: None,
            },
        )
    }
    fn live(&self, client: &Client) -> bool {
        self.registry
            .inner
            .clients
            .lock()
            .unwrap()
            .contains_key(&client.authority.client_id)
    }
    fn spawn(&self) -> String {
        let home = self._root.path().join("home");
        let response = wire(
            &self.socket,
            Operation::PaneSpawn {
                workspace_id: None,
                window_id: None,
                expected_revision: None,
                spec: PaneSpawnSpec {
                    argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                    cwd: home.clone(),
                    home,
                    isolated_environment: true,
                    title: Some("owned".into()),
                    size: spec().size,
                },
            },
        );
        assert_eq!(response["ok"], true, "{response}");
        response["result"]["pane_id"].as_str().unwrap().to_owned()
    }
    fn attach(&self, client: &Client, pane_id: &str) -> String {
        let result = self.managed(
            client,
            Operation::Pane {
                pane_id: pane_id.into(),
                request: PaneRequest::Attach { spec: spec() },
            },
        );
        assert_eq!(result["ok"], true, "{result}");
        result["result"]["view_id"].as_str().unwrap().to_owned()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = wire(&self.socket, Operation::Shutdown);
            let _ = worker.join().unwrap();
        }
    }
}
struct Client {
    stream: BufReader<UnixStream>,
    authority: ClientAuthority,
}
impl Client {
    fn event(&mut self) -> ClientEvent {
        let mut bytes = Vec::new();
        self.stream.read_until(b'\n', &mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}
fn spec() -> ViewSpec {
    ViewSpec {
        size: Size {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        visible: true,
        interactive: true,
        input: true,
        clipboard: true,
    }
}

fn acknowledge_native_turn(pane: &crate::Pane) {
    let (result, receive) = std::sync::mpsc::sync_channel(1);
    pane.commands
        .try_send(crate::Command {
            mutation: crate::Mutation::Focus(false),
            client: None,
            result,
        })
        .unwrap_or_else(|_| panic!("fixture queue full"));
    receive
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
}

#[test]
fn exact_names_persist_logical_identity_but_never_daemon_birth() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let first = Registry::open_named(root.path(), "../日本 語").unwrap();
    let identity = first.inner.session.clone();
    assert!(
        Registry::open_named(root.path(), "../日本 語").is_err(),
        "exclusive root lease"
    );
    drop(first);
    assert!(Registry::open_named(root.path(), "../日本 語 ").is_err());
    let restarted = Registry::open_named(root.path(), "../日本 語").unwrap();
    assert_eq!(identity.name, restarted.inner.session.name);
    assert_eq!(identity.session_id, restarted.inner.session.session_id);
    assert_ne!(identity.daemon_id, restarted.inner.session.daemon_id);
    let other_root = tempfile::tempdir().unwrap();
    fs::set_permissions(other_root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let other = Registry::open_named(other_root.path(), "../日本 語").unwrap();
    assert_ne!(identity.session_id, other.inner.session.session_id);
    let stored = fs::read(root.path().join("session.json")).unwrap();
    assert!(
        !String::from_utf8(stored)
            .unwrap()
            .contains(&identity.daemon_id)
    );
}

#[test]
fn session_manifest_rejects_symlinks_insecure_permissions_and_malformed_identity() {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let registry = Registry::open(root.path()).unwrap();
    drop(registry);
    let manifest = root.path().join("session.json");
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Registry::open(root.path()).is_err());
    fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink(root.path().join("absent"), &manifest).unwrap();
    assert!(Registry::open(root.path()).is_err());
    fs::remove_file(&manifest).unwrap();
    fs::write(
        &manifest,
        br#"{"version":1,"name":"bkpr","session_id":"not-a-uuid"}"#,
    )
    .unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Registry::open(root.path()).is_err());
}

#[test]
fn registration_is_not_activation_and_bad_handshakes_never_displace_clients() {
    let server = Server::new("name / ..");
    let first = server.register();
    assert_eq!(server.managed(&first, Operation::Inspect)["ok"], false);
    assert_eq!(server.activate(&first, false)["ok"], true);
    let session = server.session();
    for (session_id, daemon_id, version) in [
        ("wrong".into(), session.daemon_id.clone(), PROTOCOL_VERSION),
        (session.session_id.clone(), "wrong".into(), PROTOCOL_VERSION),
        (session.session_id, session.daemon_id, PROTOCOL_VERSION + 1),
    ] {
        assert_eq!(
            wire_request(
                &server.socket,
                Request {
                    version,
                    operation: Operation::ClientRegister {
                        session_id,
                        daemon_id
                    }
                }
            )["ok"],
            false
        );
        assert!(server.live(&first));
    }
    let pending = server.register();
    assert!(server.live(&first));
    let mut forged = pending.authority.clone();
    forged.token = "0".repeat(64);
    assert_eq!(
        wire(
            &server.socket,
            Operation::Client {
                authority: forged,
                request: Box::new(Operation::ClientActivate {
                    detach_others: true,
                    agent_socket: None,
                })
            }
        )["ok"],
        false
    );
    assert!(server.live(&first));
    assert_eq!(
        wire(
            &server.socket,
            Operation::ClientActivate {
                detach_others: true,
                agent_socket: None,
            }
        )["ok"],
        false
    );
    assert!(server.live(&first));
}

#[test]
fn takeover_retains_exact_new_client_and_repeated_activation_does_not_detach_later_arrivals() {
    let server = Server::new("first");
    let other = Server::new("second");
    let mut first = server.register();
    server.activate(&first, false);
    let second = server.register();
    server.activate(&second, false);
    let independent = other.register();
    other.activate(&independent, false);
    assert_eq!(
        server.activate(&second, true)["result"]["already_active"],
        true
    );
    assert!(server.live(&first), "takeover is not retroactively enabled");
    let newcomer = server.register();
    assert_eq!(
        server.activate(&newcomer, true)["result"]["already_active"],
        false
    );
    assert_eq!(first.event().reason, "takeover");
    assert!(!server.live(&first));
    assert!(!server.live(&second));
    assert!(server.live(&newcomer));
    assert!(other.live(&independent));
    let later = server.register();
    server.activate(&later, false);
    assert_eq!(
        server.activate(&newcomer, true)["result"]["already_active"],
        true
    );
    assert!(server.live(&later));
    assert_eq!(server.managed(&first, Operation::Inspect)["ok"], false);
    assert_eq!(other.managed(&newcomer, Operation::Inspect)["ok"], false);
}

#[test]
fn eof_and_explicit_detach_retire_only_attachment_and_release_admission() {
    let server = Server::new("eof");
    let first = server.register();
    server.activate(&first, false);
    let id = first.authority.client_id.clone();
    drop(first);
    eventually(|| {
        !server
            .registry
            .inner
            .clients
            .lock()
            .unwrap()
            .contains_key(&id)
    });
    let mut second = server.register();
    server.activate(&second, false);
    assert_eq!(server.managed(&second, Operation::ClientDetach)["ok"], true);
    assert_eq!(second.event().reason, "detached");
    assert!(!server.live(&second));
    assert_eq!(wire(&server.socket, Operation::Inspect)["ok"], true);
}

#[test]
fn managed_views_cannot_be_borrowed_by_admin_or_other_clients_and_takeover_never_terminates_panes()
{
    let server = Server::new("panes");
    let first = server.register();
    server.activate(&first, false);
    let pane_id = server.spawn();
    let view_id = server.attach(&first, &pane_id);
    let second = server.register();
    server.activate(&second, false);
    let stale = || Operation::Pane {
        pane_id: pane_id.clone(),
        request: PaneRequest::Input {
            view_id: view_id.clone(),
            data: b"not-authorized".to_vec(),
        },
    };
    assert_eq!(wire(&server.socket, stale())["ok"], false);
    assert_eq!(server.managed(&second, stale())["ok"], false);
    let newest = server.register();
    server.activate(&newest, true);
    assert_eq!(server.managed(&first, stale())["ok"], false);
    let snapshot = wire(
        &server.socket,
        Operation::Pane {
            pane_id: pane_id.clone(),
            request: PaneRequest::Inspect,
        },
    );
    assert_eq!(snapshot["result"]["lifecycle"], "running");
    assert!(snapshot["result"]["views"].as_array().unwrap().is_empty());
    assert_eq!(
        server.registry.snapshot().unwrap().panes[0].status,
        PaneStatus::Running
    );
    server.attach(&newest, &pane_id);
}

#[test]
fn client_limit_does_not_consume_control_or_agent_wait_admission() {
    let server = Server::new("limits");
    let clients = (0..MAX_CLIENTS)
        .map(|_| server.register())
        .collect::<Vec<_>>();
    let session = server.session();
    assert_eq!(
        wire(
            &server.socket,
            Operation::ClientRegister {
                session_id: session.session_id,
                daemon_id: session.daemon_id
            }
        )["ok"],
        false
    );
    assert_eq!(wire(&server.socket, Operation::Inspect)["ok"], true);
    assert_eq!(
        server.registry.inner.clients.lock().unwrap().len(),
        MAX_CLIENTS
    );
    drop(clients);
    eventually(|| server.registry.inner.clients.lock().unwrap().is_empty());
    let replacement = server.register();
    assert_eq!(server.activate(&replacement, false)["ok"], true);
}

#[test]
fn queued_native_commands_lose_client_grant_but_accepted_releases_remain_cleanup() {
    let server = Server::new("queued");
    let client = server.register();
    server.activate(&client, false);
    let home = server._root.path().join("clipboard-home");
    let created = wire(&server.socket, Operation::PaneSpawn { workspace_id: None, window_id: None, expected_revision: None, spec: PaneSpawnSpec {
        argv: vec!["/bin/sh".into(), "-c".into(),
            r"while [ ! -f query ]; do sleep 0.01; done; printf '\033]52;c;?\007'; exec sleep 60".into()],
        cwd: home.clone(), home: home.clone(), isolated_environment: true,
        title: Some("queued".into()), size: spec().size,
    } });
    assert_eq!(created["ok"], true, "{created}");
    let pane_id = created["result"]["pane_id"].as_str().unwrap();
    let view_id = server.attach(&client, pane_id);
    let pane = server.registry.inner.state.lock().unwrap().live[pane_id].clone();
    let lease = server.registry.inner.clients.lock().unwrap()[&client.authority.client_id].clone();
    let epoch = pane.shared.authority.lock().unwrap().views[&view_id].inspection_epoch;
    fs::write(home.join("query"), b"query").unwrap();
    eventually(|| pane.shared.clipboard.lock().unwrap().is_some());
    // Native feed is blocked in a real clipboard callback: these accepted
    // commands are definitely queued before takeover.
    let queued = |mutation| {
        let (result, receive) = std::sync::mpsc::sync_channel(1);
        pane.commands
            .try_send(crate::Command {
                mutation,
                client: Some(lease.clone()),
                result,
            })
            .unwrap_or_else(|_| panic!("fixture queue full"));
        receive
    };
    let input = queued(crate::Mutation::Raw(b"late".to_vec()));
    let selection = queued(crate::Mutation::CopySelection {
        view_id: view_id.clone(),
        inspection_epoch: epoch,
    });
    let release = queued(crate::Mutation::Focus(false));
    pane.shared.paste.lock().unwrap().origins.insert(
        10_000,
        crate::PasteOrigin {
            view_id: view_id.clone(),
            expires: Instant::now() + Duration::from_secs(30),
        },
    );
    let newcomer = server.register();
    assert_eq!(server.activate(&newcomer, true)["ok"], true);
    assert!(input.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
    assert!(
        selection
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_err()
    );
    assert!(
        release
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .is_ok()
    );
    assert!(!lease.is_active());
    assert!(pane.shared.authority.lock().unwrap().views.is_empty());
    assert!(pane.shared.paste.lock().unwrap().origins.is_empty());
    assert!(pane.shared.clipboard.lock().unwrap().is_none());
    assert!(
        pane.command_for_client(crate::Mutation::Raw(b"late".to_vec()), Some(lease))
            .is_err()
    );
}

#[test]
#[ignore = "subprocess-only client security fixture"]
fn foreign_client_fixture() {
    assert_eq!(std::env::var("BKPR_CLIENT_FIXTURE").unwrap(), "foreign");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let (socket, request): (PathBuf, Request) = serde_json::from_str(&line).unwrap();
    assert_eq!(wire_request(&socket, request)["ok"], false);
}

#[test]
fn copied_client_token_from_another_process_is_not_authority() {
    use std::process::{Command, Stdio};
    let server = Server::new("foreign");
    let client = server.register();
    server.activate(&client, false);
    let request = Request::new(Operation::Client {
        authority: client.authority.clone(),
        request: Box::new(Operation::Inspect),
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "registry::sessions::tests::foreign_client_fixture",
            "--ignored",
        ])
        .env("BKPR_CLIENT_FIXTURE", "foreign")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    serde_json::to_writer(&mut input, &(&server.socket, request)).unwrap();
    input.write_all(b"\n").unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    assert!(server.live(&client));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess-only client owner exit fixture"]
fn inherited_lifetime_fixture() {
    assert_eq!(std::env::var("BKPR_CLIENT_FIXTURE").unwrap(), "owner");
    let socket = PathBuf::from(std::env::var_os("BKPR_CLIENT_FIXTURE_SOCKET").unwrap());
    let session: SessionInfo =
        serde_json::from_value(wire(&socket, Operation::SessionInfo)["result"]["session"].clone())
            .unwrap();
    let mut stream = UnixStream::connect(&socket).unwrap();
    serde_json::to_writer(
        &mut stream,
        &Request::new(Operation::ClientRegister {
            session_id: session.session_id,
            daemon_id: session.daemon_id,
        }),
    )
    .unwrap();
    stream.write_all(b"\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    reader.read_until(b'\n', &mut bytes).unwrap();
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    let authority: ClientAuthority =
        serde_json::from_value(response["result"]["client"].clone()).unwrap();
    assert_eq!(
        wire(
            &socket,
            Operation::Client {
                authority,
                request: Box::new(Operation::ClientActivate {
                    detach_others: false,
                    agent_socket: None,
                })
            }
        )["ok"],
        true
    );
    let holder = unsafe { libc::fork() };
    assert!(holder >= 0);
    if holder == 0 {
        // Only async-signal-safe syscalls in this forked test child. Its inherited
        // socket must not keep its exited original owner's attachment alive.
        unsafe {
            libc::close(libc::STDOUT_FILENO);
            libc::close(libc::STDERR_FILENO);
            libc::poll(std::ptr::null_mut(), 0, 60_000);
            libc::_exit(0);
        }
    }
    println!("CLIENT_HOLDER={holder}");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_line(&mut String::new()).unwrap();
    drop(reader);
}

#[cfg(target_os = "linux")]
#[test]
fn owner_exit_revokes_even_when_a_fork_inherits_the_lifetime_socket() {
    use std::{
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        process::{Command, Stdio},
    };
    struct Holder(OwnedFd);
    impl Drop for Holder {
        fn drop(&mut self) {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
    let server = Server::new("owner");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "registry::sessions::tests::inherited_lifetime_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("BKPR_CLIENT_FIXTURE", "owner")
        .env("BKPR_CLIENT_FIXTURE_SOCKET", &server.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let holder_pid = loop {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "owner fixture did not register"
        );
        if let Some(pid) = line.trim().strip_prefix("CLIENT_HOLDER=") {
            break pid.parse::<i32>().unwrap();
        }
    };
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, holder_pid, 0) } as i32;
    assert!(fd >= 0);
    let holder = Holder(unsafe { OwnedFd::from_raw_fd(fd) });
    assert_eq!(server.registry.inner.clients.lock().unwrap().len(), 1);
    child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    assert!(child.wait().unwrap().success());
    eventually(|| server.registry.inner.clients.lock().unwrap().is_empty());
    let mut poll = libc::pollfd {
        fd: holder.0.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&mut poll, 1, 0) },
        0,
        "forked descriptor holder must still be alive"
    );
}

#[test]
fn simultaneous_takeovers_have_one_exact_survivor_and_never_reactivate_a_retired_client() {
    let server = Server::new("race");
    let first = server.register();
    let second = server.register();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let run = |authority: ClientAuthority| {
        let socket = server.socket.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            wire(
                &socket,
                Operation::Client {
                    authority,
                    request: Box::new(Operation::ClientActivate {
                        detach_others: true,
                        agent_socket: None,
                    }),
                },
            )
        })
    };
    let a = run(first.authority.clone());
    let b = run(second.authority.clone());
    barrier.wait();
    let outcomes = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(
        outcomes.iter().filter(|value| value["ok"] == true).count(),
        1
    );
    assert_ne!(server.live(&first), server.live(&second));
    assert_eq!(server.registry.inner.clients.lock().unwrap().len(), 1);
}

#[test]
fn stopping_one_named_daemon_keeps_other_session_processes_and_attachments_alive() {
    let mut first = Server::new("first / ..");
    let other = Server::new("second / ..");
    let own_pane = first.spawn();
    let other_pane = other.spawn();
    let client = other.register();
    other.activate(&client, false);
    let view = other.attach(&client, &other_pane);
    let before = other.session();
    assert_eq!(wire(&first.socket, Operation::Shutdown)["ok"], true);
    first.worker.take().unwrap().join().unwrap().unwrap();
    eventually(|| {
        first.registry.inner.state.lock().unwrap().live[&own_pane]
            .shared
            .finished
            .load(Ordering::Acquire)
    });
    assert_eq!(other.session(), before);
    assert!(other.live(&client));
    assert_eq!(
        other.managed(
            &client,
            Operation::Pane {
                pane_id: other_pane,
                request: PaneRequest::Focus {
                    view_id: view,
                    focused: true
                },
            }
        )["ok"],
        true
    );
}

#[test]
fn takeover_retires_a_captured_sizing_lease_before_native_resize() {
    let server = Server::new("sizing");
    let client = server.register();
    server.activate(&client, false);
    let pane_id = server.spawn();
    let view_id = server.attach(&client, &pane_id);
    let pane = server.registry.inner.state.lock().unwrap().live[&pane_id].clone();
    let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
    *pane.shared.resize_barrier.lock().unwrap() = Some(Arc::new(crate::ResizeBarrier {
        entered,
        resume: Mutex::new(resume_rx),
    }));
    let mut changed = spec();
    changed.size.cols = 120;
    assert_eq!(
        server.managed(
            &client,
            Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Update {
                    view_id,
                    spec: changed
                },
            }
        )["ok"],
        true
    );
    assert_eq!(
        entered_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .cols,
        120
    );
    let newcomer = server.register();
    assert_eq!(server.activate(&newcomer, true)["ok"], true);
    resume.send(()).unwrap();
    acknowledge_native_turn(&pane);
    let snapshot = wire(
        &server.socket,
        Operation::Pane {
            pane_id,
            request: PaneRequest::Inspect,
        },
    );
    assert_eq!(snapshot["result"]["frame"]["size"]["cols"], 80);
}

#[test]
fn expiry_driven_resize_cannot_outlive_its_managed_client_or_view() {
    for takeover in [false, true] {
        let server = Server::new("expiry sizing");
        let client = server.register();
        server.activate(&client, false);
        let pane_id = server.spawn();
        let view_id = server.attach(&client, &pane_id);
        let pane = server.registry.inner.state.lock().unwrap().live[&pane_id].clone();
        let mut managed = spec();
        managed.size.cols = 120;
        assert_eq!(
            server.managed(
                &client,
                Operation::Pane {
                    pane_id: pane_id.clone(),
                    request: PaneRequest::Update {
                        view_id: view_id.clone(),
                        spec: managed
                    },
                }
            )["ok"],
            true
        );
        eventually(|| pane.request(PaneRequest::Inspect).unwrap()["frame"]["size"]["cols"] == 120);
        let anonymous = wire(
            &server.socket,
            Operation::Pane {
                pane_id: pane_id.clone(),
                request: PaneRequest::Attach { spec: spec() },
            },
        )["result"]["view_id"]
            .as_str()
            .unwrap()
            .to_owned();
        eventually(|| pane.request(PaneRequest::Inspect).unwrap()["frame"]["size"]["cols"] == 80);
        let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (resume, resume_rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut authority = pane.shared.authority.lock().unwrap();
            *pane.shared.resize_barrier.lock().unwrap() = Some(Arc::new(crate::ResizeBarrier {
                entered,
                resume: Mutex::new(resume_rx),
            }));
            authority.views.get_mut(&anonymous).unwrap().last_seen =
                Instant::now() - Duration::from_secs(60);
        }
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .cols,
            120
        );
        if takeover {
            let newcomer = server.register();
            assert_eq!(server.activate(&newcomer, true)["ok"], true);
        } else {
            assert_eq!(
                server.managed(
                    &client,
                    Operation::Pane {
                        pane_id: pane_id.clone(),
                        request: PaneRequest::Detach { view_id },
                    }
                )["ok"],
                true
            );
        }
        resume.send(()).unwrap();
        acknowledge_native_turn(&pane);
        assert_eq!(
            pane.request(PaneRequest::Inspect).unwrap()["frame"]["size"]["cols"],
            80
        );
    }
}

#[test]
fn each_registry_injects_its_own_exact_session_identity_into_fresh_executions() {
    for name in ["first / 日本 語", "second .."] {
        let server = Server::new(name);
        let home = server._root.path().join("environment-home");
        server.registry.bootstrap(PaneSpawnSpec {
            argv: vec!["/bin/sh".into(), "-c".into(),
                "printf '%s\\n%s\\n' \"$BKPR_SESSION_ID\" \"$BKPR_SESSION_NAME\" >session-env; exec sleep 60".into()],
            cwd: home.clone(), home: home.clone(), isolated_environment: true,
            title: None, size: spec().size,
        }).unwrap();
        let expected = format!("{}\n{name}\n", server.session().session_id);
        eventually(|| {
            fs::read_to_string(home.join("session-env")).ok().as_deref() == Some(expected.as_str())
        });
    }
}

#[test]
fn managed_views_follow_the_client_lifetime_not_a_poll_heartbeat() {
    let server = Server::new("quiet");
    let client = server.register();
    server.activate(&client, false);
    let lease = server.registry.inner.clients.lock().unwrap()[&client.authority.client_id].clone();
    let mut authority = crate::authority::Authority::default();
    let view = authority.attach_owned(spec(), Some(lease.clone())).unwrap();
    let now = Instant::now();
    authority.views.get_mut(&view).unwrap().last_seen = now - Duration::from_secs(60);
    assert!(authority.expire(now).is_empty());
    server.registry.detach_client(&lease, "detached");
    assert_eq!(authority.expire(now), vec![view]);
}

#[test]
fn shutdown_closes_registration_before_retiring_existing_lifetimes() {
    let server = Server::new("closing");
    let mut client = server.register();
    server.activate(&client, false);
    let session = server.session();
    // Direct backend shutdown keeps transport available to exercise rejected
    // admission, rather than relying on a listener connection refusal.
    server.registry.shutdown().unwrap();
    assert_eq!(client.event().reason, "shutdown");
    assert_eq!(
        wire(
            &server.socket,
            Operation::ClientRegister {
                session_id: session.session_id,
                daemon_id: session.daemon_id,
            }
        )["ok"],
        false
    );
    assert!(server.registry.inner.clients.lock().unwrap().is_empty());
}

#[test]
fn a_closed_pending_attachment_cannot_take_over_existing_clients() {
    let server = Server::new("failed-lifetime");
    let retained = server.register();
    server.activate(&retained, false);
    let pending = server.register();
    let authority = pending.authority.clone();
    // Match RegisteredClient::close: parallel fork fixtures may briefly inherit
    // the descriptor before exec, so close alone cannot guarantee peer EOF.
    pending
        .stream
        .get_ref()
        .shutdown(std::net::Shutdown::Both)
        .unwrap();
    drop(pending);
    assert_eq!(
        wire(
            &server.socket,
            Operation::Client {
                authority,
                request: Box::new(Operation::ClientActivate {
                    detach_others: true,
                    agent_socket: None,
                }),
            }
        )["ok"],
        false
    );
    assert!(server.live(&retained));
}

#[test]
fn forwarding_tracks_selected_attachments_without_retiring_an_unrelated_signing_connection() {
    use std::{io::Read, os::unix::net::UnixListener};
    let server = Server::new("forwarding");
    server
        .registry
        .configure_agent_forwarding(&server.socket)
        .unwrap();
    let agent_path = server._root.path().join("upstream");
    let listener = UnixListener::bind(&agent_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let accept = || {
        let mut fd = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&listener),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 5000) }, 1);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    };
    let activate = |client: &Client, path: Option<PathBuf>| {
        let result = server.managed(
            client,
            Operation::ClientActivate {
                detach_others: false,
                agent_socket: path,
            },
        );
        assert_eq!(result["ok"], true, "{result}");
    };
    let forwarding_path = server._root.path().join("daemon.sock.agent");
    let inode = fs::metadata(&forwarding_path).unwrap().ino();
    let first = server.register();
    activate(&first, Some(agent_path.clone()));
    let mut old = UnixStream::connect(&forwarding_path).unwrap();
    old.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _old_upstream = accept();
    let second = server.register();
    activate(&second, Some(agent_path.clone()));
    assert_eq!(old.read(&mut [0]).unwrap(), 0);

    let mut consumer = UnixStream::connect(&forwarding_path).unwrap();
    consumer
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut upstream = accept();
    let first_id = first.authority.client_id.clone();
    drop(first);
    eventually(|| {
        !server
            .registry
            .inner
            .clients
            .lock()
            .unwrap()
            .contains_key(&first_id)
    });
    let identities_request = [0, 0, 0, 1, 11];
    consumer.write_all(&identities_request).unwrap();
    let mut received = [0; 5];
    upstream.read_exact(&mut received).unwrap();
    assert_eq!(received, identities_request);
    let empty_identities = [0, 0, 0, 5, 12, 0, 0, 0, 0];
    upstream.write_all(&empty_identities).unwrap();
    let mut response = [0; 9];
    consumer.read_exact(&mut response).unwrap();
    assert_eq!(response, empty_identities);

    let no_agent = server.register();
    activate(&no_agent, None);
    assert_eq!(consumer.read(&mut [0]).unwrap(), 0);
    let mut denied = UnixStream::connect(&forwarding_path).unwrap();
    denied
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(denied.read(&mut [0]).unwrap(), 0);
    let no_agent_id = no_agent.authority.client_id.clone();
    drop(no_agent);
    eventually(|| {
        !server
            .registry
            .inner
            .clients
            .lock()
            .unwrap()
            .contains_key(&no_agent_id)
    });
    let mut resumed = UnixStream::connect(&forwarding_path).unwrap();
    resumed
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _resumed_upstream = accept();
    let second_id = second.authority.client_id.clone();
    drop(second);
    eventually(|| {
        !server
            .registry
            .inner
            .clients
            .lock()
            .unwrap()
            .contains_key(&second_id)
    });
    assert_eq!(resumed.read(&mut [0]).unwrap(), 0);
    assert_eq!(fs::metadata(&forwarding_path).unwrap().ino(), inode);
    let mut detached = UnixStream::connect(&forwarding_path).unwrap();
    detached
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(detached.read(&mut [0]).unwrap(), 0);
}

#[test]
fn isolated_panes_do_not_gain_agent_credentials_after_client_activation() {
    let server = Server::new("isolated-agent");
    server
        .registry
        .configure_agent_forwarding(&server.socket)
        .unwrap();
    let upstream = server._root.path().join("upstream");
    let _listener = std::os::unix::net::UnixListener::bind(&upstream).unwrap();
    let client = server.register();
    let activated = server.managed(
        &client,
        Operation::ClientActivate {
            detach_others: false,
            agent_socket: Some(upstream),
        },
    );
    assert_eq!(activated["ok"], true, "{activated}");
    let output = server._root.path().join("environment");
    let home = server._root.path().join("isolated-home");
    let response = wire(
        &server.socket,
        Operation::PaneSpawn {
            workspace_id: None,
            window_id: None,
            expected_revision: None,
            spec: PaneSpawnSpec {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '%s' \"${SSH_AUTH_SOCK-unset}\" > \"$1\"; exec sleep 60".into(),
                    "fixture".into(),
                    output.to_str().unwrap().into(),
                ],
                cwd: home.clone(),
                home,
                isolated_environment: true,
                title: None,
                size: spec().size,
            },
        },
    );
    assert_eq!(response["ok"], true, "{response}");
    eventually(|| fs::read_to_string(&output).is_ok_and(|value| value == "unset"));
    assert_eq!(fs::read_to_string(output).unwrap(), "unset");
}
