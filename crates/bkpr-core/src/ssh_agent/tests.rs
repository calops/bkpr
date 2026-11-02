use super::*;
use std::{
    ffi::OsString,
    os::unix::{
        ffi::OsStringExt,
        fs::symlink,
        net::{UnixListener as Listener, UnixStream as Stream},
    },
    sync::atomic::{AtomicBool, Ordering},
};

const WAIT: Duration = Duration::from_secs(10);

fn source(path: &Path) -> Source {
    Source {
        path: path.to_owned(),
        live: Arc::new(|| true),
    }
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn stream(path: &Path) -> Stream {
    let stream = Stream::connect(path).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.set_write_timeout(Some(WAIT)).unwrap();
    stream
}

fn accept(listener: &Listener) -> Stream {
    listener.set_nonblocking(true).unwrap();
    let mut listener = UnixListener::from_std(listener.try_clone().unwrap());
    let mut poll = Poll::new().unwrap();
    poll.registry()
        .register(&mut listener, Token(0), Interest::READABLE)
        .unwrap();
    let deadline = Instant::now() + WAIT;
    let mut events = Events::with_capacity(4);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let stream: Stream = stream.into();
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(WAIT)).unwrap();
                stream.set_write_timeout(Some(WAIT)).unwrap();
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept failed: {error}"),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "relay did not open the selected upstream"
        );
        poll.poll(&mut events, Some(remaining)).unwrap();
    }
}

fn closed(stream: &mut Stream) {
    let mut bytes = [0; 4096];
    loop {
        match stream.read(&mut bytes) {
            Ok(0) => return,
            Ok(_) => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                return;
            }
            Err(error) => panic!("connection was not cancelled: {error}"),
        }
    }
}

#[test]
fn opaque_bytes_and_connection_scoped_exchanges_are_not_rewritten() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut first = stream(relay.socket());
    let mut first_upstream = accept(&listener);
    let mut second = stream(relay.socket());
    let mut second_upstream = accept(&listener);

    // Deliberately neither a supported packet set nor an echo server. Extension
    // bytes, malformed lengths and unsolicited data are all transport-owned.
    let first_request = b"\xff\0\x80\x7funknown-extension\0\xfe";
    let second_request = b"\0\0\0\x01\x1bconnection-two";
    first.write_all(first_request).unwrap();
    second.write_all(second_request).unwrap();
    let mut received = vec![0; first_request.len()];
    first_upstream.read_exact(&mut received).unwrap();
    assert_eq!(received, first_request);
    received.resize(second_request.len(), 0);
    second_upstream.read_exact(&mut received).unwrap();
    assert_eq!(received, second_request);

    first_upstream.write_all(b"provider-prompt\0\x90").unwrap();
    second_upstream.write_all(b"second-provider\xff").unwrap();
    let mut first_reply = [0; 17];
    let mut second_reply = [0; 16];
    first.read_exact(&mut first_reply).unwrap();
    second.read_exact(&mut second_reply).unwrap();
    assert_eq!(&first_reply, b"provider-prompt\0\x90");
    assert_eq!(&second_reply, b"second-provider\xff");
    first.write_all(b"approval-continuation").unwrap();
    let mut continuation = [0; 21];
    first_upstream.read_exact(&mut continuation).unwrap();
    assert_eq!(&continuation, b"approval-continuation");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn simultaneous_large_streams_preserve_bytes_under_backpressure() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    let request: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect();
    let reply: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|index| (255 - index % 253) as u8)
        .collect();
    thread::scope(|scope| {
        let mut request_writer = client.try_clone().unwrap();
        let mut reply_writer = upstream.try_clone().unwrap();
        let request_ref = &request;
        let reply_ref = &reply;
        let first = scope.spawn(move || request_writer.write_all(request_ref).unwrap());
        let second = scope.spawn(move || reply_writer.write_all(reply_ref).unwrap());
        let reader = scope.spawn(move || {
            let mut received = vec![0; request_ref.len()];
            upstream.read_exact(&mut received).unwrap();
            assert_eq!(&received, request_ref);
        });
        let mut received = vec![0; reply.len()];
        client.read_exact(&mut received).unwrap();
        assert_eq!(received, reply);
        first.join().unwrap();
        second.join().unwrap();
        reader.join().unwrap();
    });
}

#[test]
fn backpressured_connection_does_not_stall_another_connection_or_revocation() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut blocked_client = stream(relay.socket());
    let mut blocked_provider = accept(&listener);
    blocked_provider.set_nonblocking(true).unwrap();
    let mut interactive_client = stream(relay.socket());
    let mut interactive_provider = accept(&listener);
    let chunk = [0x93; 16 * 1024];
    let mut accepted = 0;
    for round in 0..8 {
        // The consumer never reads. Observe real kernel backpressure, then
        // exchange unrelated traffic while this stream remains undrained.
        loop {
            match blocked_provider.write(&chunk) {
                Ok(0) => panic!("zero-byte provider write"),
                Ok(count) => {
                    accepted += count;
                    assert!(
                        accepted < 16 * 1024 * 1024,
                        "undrained transport did not apply backpressure"
                    );
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => panic!("provider write failed: {error}"),
            }
        }
        interactive_client.write_all(&[round]).unwrap();
        let mut request = [0];
        interactive_provider.read_exact(&mut request).unwrap();
        assert_eq!(request, [round]);
        interactive_provider.write_all(&[round ^ 0xff]).unwrap();
        let mut reply = [0];
        interactive_client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [round ^ 0xff]);
    }
    assert!(accepted != 0);
    relay.select(2, None).unwrap();
    blocked_provider.set_nonblocking(false).unwrap();
    closed(&mut blocked_client);
    closed(&mut blocked_provider);
    closed(&mut interactive_client);
    closed(&mut interactive_provider);
}

#[test]
fn half_close_drains_opaque_request_and_full_reply() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    let request = vec![0xab; BUFFER_BYTES * 3 + 7];
    let reply = vec![0xcd; BUFFER_BYTES * 5 + 3];
    thread::scope(|scope| {
        let request_ref = &request;
        let reply_ref = &reply;
        let server = scope.spawn(move || {
            let mut received = Vec::new();
            upstream.read_to_end(&mut received).unwrap();
            assert_eq!(&received, request_ref);
            upstream.write_all(reply_ref).unwrap();
            upstream.shutdown(Shutdown::Write).unwrap();
        });
        client.write_all(&request).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut received = Vec::new();
        client.read_to_end(&mut received).unwrap();
        assert_eq!(received, reply);
        server.join().unwrap();
    });
}

#[test]
fn explicit_none_cancels_approval_and_listener_survives_new_selection() {
    let root = private_root();
    let first_path = root.path().join("first");
    let second_path = root.path().join("second");
    let first_listener = Listener::bind(&first_path).unwrap();
    let second_listener = Listener::bind(&second_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    let identity = Identity::of(&fs::metadata(relay.socket()).unwrap());
    relay.select(1, Some(source(&first_path))).unwrap();
    let mut client = stream(relay.socket());
    let mut first = accept(&first_listener);
    client.write_all(b"approval pending").unwrap();
    let mut request = [0; 16];
    first.read_exact(&mut request).unwrap();
    relay.select(2, None).unwrap();
    // Even if the provider sends after select returns, no retired reply is
    // forwarded and no partially sent request is moved to the next provider.
    let _ = first.write_all(b"retired reply");
    let mut byte = [0];
    match client.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
        result => panic!("retired provider replied: {result:?}"),
    }
    closed(&mut first);
    closed(&mut stream(relay.socket()));
    relay.select(3, Some(source(&second_path))).unwrap();
    let mut replacement = stream(relay.socket());
    let mut second = accept(&second_listener);
    second.write_all(b"new").unwrap();
    let mut reply = [0; 3];
    replacement.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"new");
    assert!(Identity::of(&fs::metadata(relay.socket()).unwrap()) == identity);
}

#[test]
fn stale_publication_cannot_retake_route_and_new_same_path_epoch_cancels() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(9, Some(source(&upstream_path))).unwrap();
    relay
        .select(8, Some(source(&root.path().join("missing"))))
        .unwrap();
    relay.select(9, None).unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    relay.select(10, Some(source(&upstream_path))).unwrap();
    closed(&mut client);
    closed(&mut upstream);
    let mut replacement = stream(relay.socket());
    let mut fresh = accept(&listener);
    fresh.write_all(b"fresh").unwrap();
    let mut reply = [0; 5];
    replacement.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"fresh");
}

#[test]
fn unavailable_newest_source_masks_older_and_can_recover_without_reattaching() {
    let root = private_root();
    let first_path = root.path().join("first");
    let listener = Listener::bind(&first_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&first_path))).unwrap();
    let mut old_client = stream(relay.socket());
    let mut old_upstream = accept(&listener);
    let later_path = root.path().join("later");
    relay.select(2, Some(source(&later_path))).unwrap();
    closed(&mut old_client);
    closed(&mut old_upstream);
    closed(&mut stream(relay.socket()));

    let replacement = Listener::bind(&later_path).unwrap();
    let mut client = stream(relay.socket());
    let mut provider = accept(&replacement);
    provider.write_all(b"recovered").unwrap();
    let mut reply = [0; 9];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"recovered");
}

#[test]
fn dead_source_rejects_admission_and_stops_existing_io() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let check = live.clone();
    relay
        .select(
            1,
            Some(Source {
                path: upstream_path,
                live: Arc::new(move || check.load(Ordering::Acquire)),
            }),
        )
        .unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    live.store(false, Ordering::Release);
    let _ = client.write_all(b"must not reach provider");
    let mut byte = [0];
    match upstream.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
        result => panic!("retired attachment sent data: {result:?}"),
    }
    closed(&mut client);
    closed(&mut stream(relay.socket()));
}

#[test]
fn recursive_routes_and_inode_aliases_fail_closed() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    assert!(relay.select(2, Some(source(relay.socket()))).is_err());
    closed(&mut client);
    closed(&mut upstream);
    closed(&mut stream(relay.socket()));
    let alias = root.path().join("alias");
    symlink(relay.socket(), &alias).unwrap();
    relay.select(3, Some(source(&alias))).unwrap();
    closed(&mut stream(relay.socket()));
    let hardlink = root.path().join("hardlink");
    fs::hard_link(relay.socket(), &hardlink).unwrap();
    relay.select(4, Some(source(&hardlink))).unwrap();
    closed(&mut stream(relay.socket()));
    relay.shutdown().unwrap();
    assert!(
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(fs::metadata(&hardlink).unwrap().file_type().is_socket());
}

#[test]
fn connection_admission_is_bounded_and_shutdown_closes_every_owned_stream() {
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut clients = Vec::new();
    let mut upstreams = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        clients.push(stream(relay.socket()));
        upstreams.push(accept(&listener));
    }
    closed(&mut stream(relay.socket()));
    relay.shutdown().unwrap();
    relay.shutdown().unwrap();
    assert!(!relay.socket().exists());
    for client in &mut clients {
        closed(client);
    }
    for upstream in &mut upstreams {
        closed(upstream);
    }
    assert!(relay.select(2, None).is_err());
}

#[test]
fn stale_recovery_is_lease_owned_and_preserves_live_or_foreign_paths() {
    let root = private_root();
    let path = root.path().join("forward");
    let listener = Listener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let identity = Identity::of(&fs::metadata(&path).unwrap());
    assert!(Relay::bind(&path).is_err());
    assert!(Identity::of(&fs::metadata(&path).unwrap()) == identity);
    drop(listener);
    let relay = Relay::bind(&path).unwrap();
    assert!(Relay::bind(&path).is_err());
    relay.shutdown().unwrap();
    let replacement = Relay::bind(&path).unwrap();
    drop(replacement);
    fs::write(&path, b"unrelated").unwrap();
    assert!(Relay::bind(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"unrelated");
    fs::remove_file(&path).unwrap();
    let target = root.path().join("target");
    let _listener = Listener::bind(&target).unwrap();
    symlink(&target, &path).unwrap();
    assert!(Relay::bind(&path).is_err());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn explicit_lease_unlock_survives_an_inherited_open_description() {
    let root = private_root();
    let path = root.path().join("forward");
    let lease = SocketLease::acquire(&path).unwrap();
    let inherited = lease.0.try_clone().unwrap();
    assert!(SocketLease::acquire(&path).is_err());
    drop(lease);
    let _replacement = SocketLease::acquire(&path).unwrap();
    drop(inherited);
}

#[test]
fn cleanup_never_unlinks_a_replacement_inode() {
    let root = private_root();
    let path = root.path().join("forward");
    let relay = Relay::bind(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let _replacement = Listener::bind(&path).unwrap();
    let identity = Identity::of(&fs::metadata(&path).unwrap());
    relay.shutdown().unwrap();
    drop(relay);
    assert!(Identity::of(&fs::metadata(&path).unwrap()) == identity);
}

#[test]
fn non_utf8_socket_paths_are_preserved() {
    let root = private_root();
    let upstream_path = root
        .path()
        .join(OsString::from_vec(b"upstream-\xff".to_vec()));
    let path = root
        .path()
        .join(OsString::from_vec(b"forward-\xfe".to_vec()));
    let listener = Listener::bind(&upstream_path).unwrap();
    let relay = Relay::bind(&path).unwrap();
    assert_eq!(relay.socket(), path);
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut client = stream(relay.socket());
    let mut upstream = accept(&listener);
    upstream.write_all(b"raw path").unwrap();
    let mut reply = [0; 8];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"raw path");
}

#[test]
fn non_socket_source_is_unavailable_without_modifying_the_path() {
    let root = private_root();
    let relay = Relay::bind(&root.path().join("forward")).unwrap();
    let unavailable = root.path().join("not-a-socket");
    fs::write(&unavailable, b"unrelated").unwrap();
    relay.select(1, Some(source(&unavailable))).unwrap();
    closed(&mut stream(relay.socket()));
    assert_eq!(fs::read(&unavailable).unwrap(), b"unrelated");
}

#[test]
fn stuck_admission_cannot_stall_other_traffic_revocation_or_shutdown() {
    // Always release the injected kernel-lookup stand-in, including if an
    // earlier assertion fails; no blocked fixture worker survives this test.
    struct Release(Sender<()>);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }
    let root = private_root();
    let upstream_path = root.path().join("upstream");
    let listener = Listener::bind(&upstream_path).unwrap();
    let (entered, wait_entered) = crossbeam_channel::bounded(1);
    let (release, wait_release) = crossbeam_channel::bounded(1);
    let (resumed, wait_resumed) = crossbeam_channel::bounded(1);
    let release = Release(release);
    let first = AtomicBool::new(true);
    let hook: AdmissionHook = Arc::new(move |_| {
        if first.swap(false, Ordering::AcqRel) {
            entered.send(()).unwrap();
            wait_release.recv().unwrap();
            let _ = resumed.send(());
        }
    });
    let relay = Arc::new(Relay::bind_inner(&root.path().join("forward"), Some(hook)).unwrap());
    relay.select(1, Some(source(&upstream_path))).unwrap();
    let mut stalled = stream(relay.socket());
    wait_entered.recv_timeout(WAIT).unwrap();

    // The other fixed worker can admit this connection while the first is
    // stuck before its pathname lookup completes.
    let mut independent = stream(relay.socket());
    let mut provider = accept(&listener);
    independent.write_all(b"unrelated request").unwrap();
    let mut request = [0; 17];
    provider.read_exact(&mut request).unwrap();
    assert_eq!(&request, b"unrelated request");
    provider.write_all(b"unrelated response").unwrap();
    let mut reply = [0; 18];
    independent.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"unrelated response");
    relay.select(2, None).unwrap();
    closed(&mut stalled);
    closed(&mut independent);
    closed(&mut provider);

    relay.select(3, Some(source(&upstream_path))).unwrap();
    let mut replacement = stream(relay.socket());
    let mut replacement_provider = accept(&listener);
    replacement_provider.write_all(b"fresh").unwrap();
    let mut fresh = [0; 5];
    replacement.read_exact(&mut fresh).unwrap();
    assert_eq!(&fresh, b"fresh");
    let stopping = relay.clone();
    let (finished, wait_finished) = crossbeam_channel::bounded(1);
    let shutdown = thread::spawn(move || {
        let _ = finished.send(stopping.shutdown());
    });
    let outcome = wait_finished.recv_timeout(WAIT);
    // Capture the result before releasing the stuck worker, but release before
    // asserting: an implementation that incorrectly joins must fail, not hang.
    drop(release);
    wait_resumed.recv_timeout(WAIT).unwrap();
    shutdown.join().unwrap();
    outcome.unwrap().unwrap();
    closed(&mut replacement);
    closed(&mut replacement_provider);
    assert!(!relay.socket().exists());
}
