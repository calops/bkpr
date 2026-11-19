use super::*;
use bkpr_protocol::administration::{Identity, Request as AdminRequest, Response as AdminResponse};

fn identity(service: &Service) -> Identity {
    match service {
        Service::Host(host) => Identity::Host {
            daemon_id: host.info().daemon_id.clone(),
        },
        Service::Session(registry) => {
            let session = registry.session_info();
            Identity::Session {
                daemon_id: session.daemon_id.clone(),
                session_id: session.session_id.clone(),
                session_instance_id: session.session_instance_id.clone(),
            }
        }
    }
}

fn dispatch(
    service: &Service,
    request: AdminRequest,
    shutdown: &AtomicBool,
    closing: &AtomicBool,
    startup: &StartupGate,
    stop: &mut bool,
) -> Result<AdminResponse> {
    match request {
        AdminRequest::Inspect {} => Ok(AdminResponse::Identified {
            identity: identity(service),
        }),
        AdminRequest::Stop { expected } => {
            anyhow::ensure!(
                expected == identity(service),
                "administration endpoint identity or role changed"
            );
            anyhow::ensure!(
                !shutdown.load(Ordering::Acquire),
                SessionClosing("endpoint is shutting down")
            );
            *stop = true;
            closing.store(true, Ordering::Release);
            startup.cancel();
            if let Service::Host(host) = service {
                host.request_shutdown();
            }
            service.shutdown()?;
            Ok(AdminResponse::Stopped {})
        }
    }
}

pub(super) struct Resources<'a> {
    pub(super) endpoint: &'a std::sync::Weak<Service>,
    pub(super) responses: &'a Arc<ResponseDrain>,
    pub(super) response_work: &'a WorkStages,
    pub(super) response_buffers: &'a FrameBuffers,
    pub(super) shutdown: &'a AtomicBool,
    pub(super) closing: &'a AtomicBool,
    pub(super) startup: &'a StartupGate,
    pub(super) wake: &'a UnixStream,
}

pub(super) fn serve(
    stream: &mut UnixStream,
    request: Result<(AdminRequest, capacity::BufferReservation<'_>)>,
    reader: &mut PredecodeReader,
    resources: Resources<'_>,
) {
    let mut pending = None;
    let mut service = None;
    let mut stop = false;
    let result = (|| {
        let (request, _reservation) = request?;
        pending = Some(reader.admit(resources.responses)?);
        service = resources.endpoint.upgrade();
        let service = service.as_ref().context("endpoint is shutting down")?;
        dispatch(
            service,
            request,
            resources.shutdown,
            resources.closing,
            resources.startup,
            &mut stop,
        )
    })();
    if pending.is_none() && resources.shutdown.load(Ordering::Acquire) {
        return;
    }
    let reply = result.unwrap_or_else(|error| {
        let mut error = format!("{error:#}");
        let mut end = error.len().min(1024);
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        error.truncate(end);
        AdminResponse::Rejected { error }
    });
    let stage = resources.response_work.enter();
    let mut bytes =
        [0; bkpr_protocol::administration::HEADER_LEN + bkpr_protocol::administration::MAX_PAYLOAD];
    let mut output = std::io::Cursor::new(bytes.as_mut_slice());
    if bkpr_protocol::administration::write_response(&mut output, &reply).is_ok() {
        let length = output.position() as usize;
        if let Ok(_reservation) = resources.response_buffers.reserve(length) {
            drop(stage);
            let _ = ResponseWriter {
                stream,
                deadline: std::time::Instant::now() + Duration::from_secs(3),
                progress: std::time::Instant::now(),
                stall: None,
                active: None,
                sent: 0,
            }
            .write_all(&bytes[..length]);
        }
    }
    drop(service);
    drop(pending);
    if stop {
        resources.shutdown.store(true, Ordering::Release);
        let _ = (&*resources.wake).write_all(b"\0");
        resources.startup.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> StartupGate {
        StartupGate {
            complete: Mutex::new(false),
            changed: Condvar::new(),
            cancelled: AtomicBool::new(false),
        }
    }

    #[test]
    fn host_endpoint_rejects_session_role_before_any_shutdown_effect() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let host = crate::daemon::HostDaemon::bind(crate::daemon::HostConfig {
            socket: root.path().join("host.sock"),
            state_root: root.path().join("state"),
            bkpr: std::env::current_exe().unwrap(),
        })
        .unwrap();
        let service = Service::Host(host.clone());
        let shutdown = AtomicBool::new(false);
        let closing = AtomicBool::new(false);
        let startup = gate();
        let mut stop = false;
        let expected = Identity::Session {
            daemon_id: host.info().daemon_id.clone(),
            session_id: "session".into(),
            session_instance_id: "instance".into(),
        };
        assert!(
            dispatch(
                &service,
                AdminRequest::Stop { expected },
                &shutdown,
                &closing,
                &startup,
                &mut stop
            )
            .is_err()
        );
        assert!(!stop && !host.stop_requested() && !closing.load(Ordering::Acquire));
        assert!(!startup.cancelled.load(Ordering::Acquire));
        assert_eq!(
            dispatch(
                &service,
                AdminRequest::Stop {
                    expected: identity(&service)
                },
                &shutdown,
                &closing,
                &startup,
                &mut stop
            )
            .unwrap(),
            AdminResponse::Stopped {}
        );
        assert!(stop && host.stop_requested());
    }

    #[test]
    fn administration_shares_listener_and_retires_partial_readers_without_relaxing_native_family() {
        use bkpr_protocol::administration as admin;
        let root = tempfile::Builder::new().prefix("ba").tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let expected = identity(&Service::Session(registry.clone()));
        let socket = root.path().join("s");
        let bound = bind(&socket).unwrap();
        let worker = thread::spawn(move || bound.serve(registry));
        let exchange = |request: AdminRequest| {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            admin::write_request(&mut stream, &request).unwrap();
            admin::read_response(&mut stream).unwrap()
        };
        assert_eq!(
            exchange(AdminRequest::Inspect {}),
            AdminResponse::Identified {
                identity: expected.clone()
            }
        );
        let wrong = match &expected {
            Identity::Session { daemon_id, .. } => Identity::Host {
                daemon_id: daemon_id.clone(),
            },
            _ => unreachable!(),
        };
        assert!(matches!(
            exchange(AdminRequest::Stop { expected: wrong }),
            AdminResponse::Rejected { .. }
        ));
        let mut native = UnixStream::connect(&socket).unwrap();
        native
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut codec = Codec::new();
        let mut frame = wire::encode_frame(
            Kind::Request,
            codec
                .encode_request(&Request::new(Operation::Shutdown))
                .unwrap(),
        )
        .unwrap();
        frame[4..8].copy_from_slice(&(PROTOCOL_VERSION + 1).to_le_bytes());
        native.write_all(&frame).unwrap();
        let bytes = wire::read_frame(&mut native, Kind::Response, MAX_RESPONSE).unwrap();
        assert!(!wire::decode_response(&bytes).unwrap().ok);
        assert_eq!(
            exchange(AdminRequest::Inspect {}),
            AdminResponse::Identified {
                identity: expected.clone()
            }
        );
        let mut partial = UnixStream::connect(&socket).unwrap();
        partial
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut frame = Vec::new();
        admin::write_request(&mut frame, &AdminRequest::Inspect {}).unwrap();
        partial.write_all(&frame[..admin::HEADER_LEN + 1]).unwrap();
        assert_eq!(
            exchange(AdminRequest::Stop { expected }),
            AdminResponse::Stopped {}
        );
        let mut byte = [0; 1];
        assert!(matches!(partial.read(&mut byte), Ok(0) | Err(_)));
        worker.join().unwrap().unwrap();
        assert!(!socket.exists());
    }

    #[test]
    fn wrong_ids_and_role_never_admit_shutdown_or_cancel_startup() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        let service = Service::Session(registry.clone());
        let actual = identity(&service);
        let Identity::Session {
            daemon_id,
            session_id,
            session_instance_id,
        } = actual.clone()
        else {
            unreachable!()
        };
        let wrong = [
            Identity::Host {
                daemon_id: daemon_id.clone(),
            },
            Identity::Session {
                daemon_id: "wrong".into(),
                session_id: session_id.clone(),
                session_instance_id: session_instance_id.clone(),
            },
            Identity::Session {
                daemon_id: daemon_id.clone(),
                session_id: "wrong".into(),
                session_instance_id: session_instance_id.clone(),
            },
            Identity::Session {
                daemon_id,
                session_id,
                session_instance_id: "wrong".into(),
            },
        ];
        let shutdown = AtomicBool::new(false);
        let closing = AtomicBool::new(false);
        let startup = gate();
        let mut stop = false;
        for expected in wrong {
            assert!(
                dispatch(
                    &service,
                    AdminRequest::Stop { expected },
                    &shutdown,
                    &closing,
                    &startup,
                    &mut stop
                )
                .is_err()
            );
            assert!(!stop);
            assert!(!closing.load(Ordering::Acquire));
            assert!(!startup.cancelled.load(Ordering::Acquire));
            registry.request_admission().unwrap();
        }
        assert_eq!(
            dispatch(
                &service,
                AdminRequest::Inspect {},
                &shutdown,
                &closing,
                &startup,
                &mut stop
            )
            .unwrap(),
            AdminResponse::Identified {
                identity: actual.clone()
            }
        );
        assert_eq!(
            dispatch(
                &service,
                AdminRequest::Stop { expected: actual },
                &shutdown,
                &closing,
                &startup,
                &mut stop
            )
            .unwrap(),
            AdminResponse::Stopped {}
        );
        assert!(
            stop && closing.load(Ordering::Acquire) && startup.cancelled.load(Ordering::Acquire)
        );
    }

    #[test]
    fn poisoned_persistence_does_not_hide_identity_or_prevent_owned_cleanup() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let registry = Registry::open(root.path()).unwrap();
        registry
            .request(Request::new(Operation::WorkspaceCreate {
                name: Some("before".into()),
                initial: None,
            }))
            .unwrap();
        std::fs::remove_file(root.path().join("registry.json")).unwrap();
        std::fs::create_dir(root.path().join("registry.json")).unwrap();
        assert!(
            registry
                .request(Request::new(Operation::WorkspaceCreate {
                    name: Some("poison".into()),
                    initial: None
                }))
                .is_err()
        );
        assert!(registry.request_health().is_err());
        let service = Service::Session(registry.clone());
        let shutdown = AtomicBool::new(false);
        let closing = AtomicBool::new(false);
        let startup = gate();
        let mut stop = false;
        let expected = identity(&service);
        assert_eq!(
            dispatch(
                &service,
                AdminRequest::Inspect {},
                &shutdown,
                &closing,
                &startup,
                &mut stop
            )
            .unwrap(),
            AdminResponse::Identified {
                identity: expected.clone()
            }
        );
        let result = dispatch(
            &service,
            AdminRequest::Stop { expected },
            &shutdown,
            &closing,
            &startup,
            &mut stop,
        );
        assert!(stop && closing.load(Ordering::Acquire));
        assert_eq!(result.is_ok(), registry.shutdown().is_ok());
        assert!(registry.request_admission().is_err());
    }
}
