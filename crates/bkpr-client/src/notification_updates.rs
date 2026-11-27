//! Cancellable ordered notification observations for one attached endpoint.
use crate::{
    observation::{Observer, Publisher},
    transport::Endpoint,
};
use anyhow::{Context, Result};
use bkpr_protocol::{
    MAX_NOTIFICATION_WAIT_MS, NotificationPage, NotificationQuery, NotificationRequest, Operation,
};
pub(crate) fn start(endpoint: Endpoint) -> Result<Observer<NotificationPage>> {
    Observer::spawn(
        "bkpr-notifications",
        endpoint,
        |endpoint, mut publisher| async move {
            observe(&endpoint, &mut publisher).await;
        },
    )
}

async fn observe(endpoint: &Endpoint, publisher: &mut Publisher<NotificationPage>) {
    let mut after = None;
    let mut store_id = None;
    let mut previous_status = None;
    loop {
        let operation = Operation::Notifications {
            request: NotificationRequest::Subscribe {
                query: NotificationQuery {
                    include_channel_attention: true,
                    ..NotificationQuery::default()
                },
                wait_ms: if after.is_none() {
                    0
                } else {
                    MAX_NOTIFICATION_WAIT_MS
                },
                after: after.take(),
            },
        };
        let value = endpoint
            .checked_observation_async(operation, &publisher.cancellation)
            .await
            .and_then(|value| {
                serde_json::from_value::<NotificationPage>(value)
                    .context("decoding notification subscription")
            });
        let failed = value.is_err();
        if let Ok(page) = &value {
            if store_id
                .as_ref()
                .is_some_and(|store| store != &page.cursor.store_id)
            {
                after = None;
                store_id = None;
                previous_status = None;
                continue;
            }
            if store_id.is_none() {
                store_id = Some(page.cursor.store_id.clone());
            }
            after = Some(page.cursor.clone());
            let unchanged = previous_status
                .as_ref()
                .is_some_and(|(health, gap)| health == &page.health && gap == &page.gap);
            if page.notifications.is_empty() && unchanged {
                continue;
            }
            previous_status = Some((page.health.clone(), page.gap));
        }
        if !publisher.send(endpoint, value).await || failed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::test_wire::{read_request, write_response};
    use bkpr_protocol::channels::{ChannelId, MessageId};
    use bkpr_protocol::{
        ChannelAttentionKind, ChannelNotificationSource, HarnessKind, Notification,
        NotificationCursor, NotificationHealth, NotificationKind, NotificationSource,
        PROTOCOL_VERSION, PaneNotificationSource, ProcessIdentity, Response, SessionInfo,
    };
    use std::{
        io::Read,
        os::unix::net::{UnixListener, UnixStream},
        thread,
        time::Duration,
    };
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn page(store: &str, sequence: u64, records: &[u64]) -> NotificationPage {
        NotificationPage {
            notifications: records
                .iter()
                .map(|&sequence| Notification {
                    sequence,
                    id: format!("record-{sequence}"),
                    source: NotificationSource::Pane(PaneNotificationSource {
                        session: SessionInfo {
                            name: "fixture".into(),
                            session_id: "session".into(),
                            daemon_id: "daemon".into(),
                            session_instance_id: "instance".into(),
                        },
                        workspace_id: None,
                        window_id: None,
                        pane_id: "pane".into(),
                        harness: HarnessKind::Omp,
                        process: ProcessIdentity {
                            pid: 1,
                            birth: "birth".into(),
                        },
                        launch_id: None,
                        binding_generation: None,
                    }),
                    kind: NotificationKind::Attention,
                    attention_kind: Some("question".into()),
                    observed_at_ms: 1,
                    expires_at_ms: 100,
                })
                .collect(),
            cursor: NotificationCursor {
                store_id: store.into(),
                sequence,
                loss_generation: 0,
            },
            gap: false,
            has_more: false,
            health: NotificationHealth {
                healthy: true,
                error: None,
                dropped_observations: 0,
                retained: 100,
            },
        }
    }

    fn subscribe(listener: &UnixListener, after: Option<(&str, u64)>) -> UnixStream {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let Operation::Notifications {
            request:
                NotificationRequest::Subscribe {
                    query,
                    after: requested,
                    wait_ms,
                },
        } = read_request(&mut stream).unwrap().operation
        else {
            panic!("expected notification subscription");
        };
        assert_eq!(
            query,
            NotificationQuery {
                include_channel_attention: true,
                ..NotificationQuery::default()
            },
        );
        assert_eq!(
            requested
                .as_ref()
                .map(|cursor| (cursor.store_id.as_str(), cursor.sequence)),
            after,
        );
        assert_eq!(
            wait_ms,
            if after.is_none() {
                0
            } else {
                MAX_NOTIFICATION_WAIT_MS
            },
        );
        stream
    }

    fn respond(mut stream: UnixStream, page: NotificationPage) {
        write_response(
            &mut stream,
            &Response {
                version: PROTOCOL_VERSION,
                ok: true,
                result: Some(serde_json::to_value(page).unwrap().into()),
                error: None,
                failure: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn live_baseline_advances_silent_pages_preserves_records_and_rebaselines_new_store() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("notifications.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (waiting, waiting_rx) = tokio::sync::oneshot::channel();
        let server = thread::spawn(move || {
            respond(subscribe(&listener, None), page("first", 40, &[]));
            respond(
                subscribe(&listener, Some(("first", 40))),
                page("first", 41, &[]),
            );
            respond(
                subscribe(&listener, Some(("first", 41))),
                page("first", 42, &[42]),
            );
            let mut channel_page = page("first", 43, &[]);
            channel_page.notifications.push(Notification {
                sequence: 43,
                id: "record-43".into(),
                source: NotificationSource::Channel(ChannelNotificationSource {
                    session: SessionInfo {
                        name: "fixture".into(),
                        session_id: "session".into(),
                        daemon_id: "daemon".into(),
                        session_instance_id: "instance".into(),
                    },
                    workspace_id: "workspace".into(),
                    channel_id: ChannelId("channel".into()),
                    message_id: MessageId("message".into()),
                    sequence: 8,
                    intent: ChannelAttentionKind::Question,
                }),
                kind: NotificationKind::Attention,
                attention_kind: None,
                observed_at_ms: 1,
                expires_at_ms: 100,
            });
            respond(subscribe(&listener, Some(("first", 42))), channel_page);
            respond(
                subscribe(&listener, Some(("first", 43))),
                page("replacement", 100, &[100]),
            );
            respond(subscribe(&listener, None), page("replacement", 110, &[]));
            let mut pending = subscribe(&listener, Some(("replacement", 110)));
            waiting.send(()).unwrap();
            assert_eq!(pending.read(&mut [0]).unwrap(), 0);
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock));
        });
        let endpoint = Endpoint::unmanaged(&socket);
        let (sender, mut receiver) = mpsc::channel(1);
        let (_wake, notify) = UnixStream::pair().unwrap();
        notify.set_nonblocking(true).unwrap();
        let cancellation = CancellationToken::new();
        let mut publisher = Publisher::new(sender, notify, cancellation.clone());
        runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(observe(&endpoint, &mut publisher), async {
                    let baseline = receiver.recv().await.unwrap().unwrap();
                    assert_eq!(baseline.cursor.sequence, 40);
                    assert!(baseline.notifications.is_empty());
                    let pane = receiver.recv().await.unwrap().unwrap();
                    assert_eq!(pane.notifications.len(), 1);
                    assert_eq!(pane.notifications[0].sequence, 42);
                    assert_eq!(pane.notifications[0].source.pane_id(), Some("pane"));
                    let channel = receiver.recv().await.unwrap().unwrap();
                    assert_eq!(channel.notifications.len(), 1);
                    assert_eq!(channel.notifications[0].sequence, 43);
                    let NotificationSource::Channel(source) = &channel.notifications[0].source
                    else {
                        panic!("expected channel notification");
                    };
                    assert_eq!(source.intent, ChannelAttentionKind::Question);
                    assert_eq!(source.sequence, 8);
                    let baseline = receiver.recv().await.unwrap().unwrap();
                    assert_eq!(baseline.cursor.store_id, "replacement");
                    assert_eq!(baseline.cursor.sequence, 110);
                    assert!(baseline.notifications.is_empty());
                    waiting_rx.await.unwrap();
                    cancellation.cancel();
                });
            })
            .await
            .unwrap();
        });
        drop(publisher);
        assert!(receiver.try_recv().is_err());
        server.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_full_queue_without_another_subscription() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("notifications.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (queued, queued_rx) = tokio::sync::oneshot::channel();
        let (finished, finished_rx) = std::sync::mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            respond(subscribe(&listener, None), page("store", 40, &[]));
            respond(
                subscribe(&listener, Some(("store", 40))),
                page("store", 41, &[41]),
            );
            queued.send(()).unwrap();
            finished_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            listener.set_nonblocking(true).unwrap();
            assert!(matches!(listener.accept(), Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock));
        });
        let endpoint = Endpoint::unmanaged(&socket);
        let (sender, mut receiver) = mpsc::channel(1);
        let (_wake, notify) = UnixStream::pair().unwrap();
        notify.set_nonblocking(true).unwrap();
        let cancellation = CancellationToken::new();
        let mut publisher = Publisher::new(sender, notify, cancellation.clone());
        runtime().block_on(async {
            tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(observe(&endpoint, &mut publisher), async {
                    queued_rx.await.unwrap();
                    cancellation.cancel();
                });
            })
            .await
            .unwrap();
        });
        drop(publisher);
        let baseline = receiver.try_recv().unwrap().unwrap();
        assert_eq!(baseline.cursor.sequence, 40);
        assert!(receiver.try_recv().is_err());
        finished.send(()).unwrap();
        server.join().unwrap();
    }
}
