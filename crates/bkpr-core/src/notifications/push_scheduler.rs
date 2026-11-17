use super::{
    push_store::Job,
    push_transport::{Publisher, PushSendOutcome},
};
use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    collections::BTreeMap,
    future::{Future, poll_fn},
    pin::pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Poll, Waker},
    thread,
};

#[derive(Default)]
struct Cancellation {
    canceled: AtomicBool,
    waker: Mutex<Option<Waker>>,
}
impl Cancellation {
    fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct Running {
    destination: String,
    generation: u64,
    cancel: Arc<Cancellation>,
}
/// Four isolated sends; the SQLite owner alone claims and completes durable work.
pub(super) struct Scheduler {
    jobs: Sender<(Job, Arc<Cancellation>)>,
    pub results: Receiver<(String, PushSendOutcome)>,
    running: BTreeMap<String, Running>,
}
impl Scheduler {
    pub fn new() -> Result<Self> {
        let (jobs, receiver) = bounded::<(Job, Arc<Cancellation>)>(4);
        let (sender, results) = bounded(4);
        for index in 0..4 {
            let receiver = receiver.clone();
            let sender = sender.clone();
            thread::Builder::new()
                .name(format!("bkpr-push-{index}"))
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .max_blocking_threads(1)
                        .enable_all()
                        .build();
                    let mut publisher: Option<Publisher> = None;
                    while let Ok((job, cancel)) = receiver.recv() {
                        let outcome = if cancel.canceled.load(Ordering::Acquire) {
                            PushSendOutcome::NotSent {
                                reason: "destination_retired_before_dispatch".into(),
                            }
                        } else {
                            if publisher.is_none() {
                                publisher = Publisher::new(&job.private_pem).ok();
                            }
                            match (&runtime, &publisher) {
                                (Ok(runtime), Some(publisher)) => runtime.block_on(async {
                                    let send = publisher.send(
                                        &job.subscription,
                                        &job.policy,
                                        &job.payload,
                                        job.ttl,
                                    );
                                    let mut send = pin!(send);
                                    poll_fn(|cx| {
                                        *cancel.waker.lock().unwrap() = Some(cx.waker().clone());
                                        if cancel.canceled.load(Ordering::Acquire) {
                                            return Poll::Ready(PushSendOutcome::Unknown {
                                                reason: "dispatch_canceled".into(),
                                            });
                                        }
                                        send.as_mut().poll(cx)
                                    })
                                    .await
                                }),
                                _ => PushSendOutcome::NotSent {
                                    reason: "publisher_unavailable".into(),
                                },
                            }
                        };
                        if sender.send((job.id, outcome)).is_err() {
                            break;
                        }
                    }
                    if let Ok(runtime) = runtime {
                        runtime.shutdown_timeout(std::time::Duration::from_millis(100));
                    }
                })
                .context("starting isolated push network worker")?;
        }
        Ok(Self {
            jobs,
            results,
            running: BTreeMap::new(),
        })
    }
    pub fn available(&self) -> bool {
        self.running.len() < 4
    }
    pub fn submit(&mut self, job: Job) -> Result<()> {
        let cancel = Arc::new(Cancellation::default());
        self.running.insert(
            job.id.clone(),
            Running {
                destination: job.destination_id.clone(),
                generation: job.generation,
                cancel: cancel.clone(),
            },
        );
        self.jobs
            .try_send((job, cancel))
            .map_err(|_| anyhow::anyhow!("push dispatcher unavailable after durable claim"))?;
        Ok(())
    }
    pub fn complete(&mut self, id: &str) {
        self.running.remove(id);
    }
    pub fn fence(&self, store: &super::storage::Store) -> Result<()> {
        for running in self.running.values() {
            if !store.push_live(&running.destination, running.generation)? {
                running.cancel.cancel();
            }
        }
        Ok(())
    }
}
impl Drop for Scheduler {
    fn drop(&mut self) {
        for running in self.running.values() {
            running.cancel.cancel();
        }
    }
}
