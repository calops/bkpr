use crate::transport::Endpoint;
use anyhow::{Context, Result};
use std::{
    future::Future,
    io::{Read, Write},
    os::unix::net::UnixStream,
    thread,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// A bounded, cancellable observation worker with a pollable publication edge.
pub(crate) struct Observer<T> {
    receiver: mpsc::Receiver<Result<T>>,
    pub(crate) wake: UnixStream,
    cancellation: CancellationToken,
}

impl<T: Send + 'static> Observer<T> {
    pub(crate) fn spawn<F, Fut>(name: &str, endpoint: Endpoint, run: F) -> Result<Self>
    where
        F: FnOnce(Endpoint, Publisher<T>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let (wake, mut notify) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        notify.set_nonblocking(true)?;
        let (sender, receiver) = mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let stopped = cancellation.clone();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        if !stopped.is_cancelled() && sender.try_send(Err(error.into())).is_ok() {
                            let _ = notify.write_all(&[1]);
                        }
                        return;
                    }
                };
                runtime.block_on(run(endpoint, Publisher::new(sender, notify, stopped)));
            })
            .with_context(|| format!("starting {name} observer"))?;
        Ok(Self {
            receiver,
            wake,
            cancellation,
        })
    }

    pub(crate) fn next(&mut self) -> Option<Result<T>> {
        let mut bytes = [0; 128];
        while matches!(self.wake.read(&mut bytes), Ok(count) if count > 0) {}
        self.receiver.try_recv().ok()
    }
}

impl<T> Drop for Observer<T> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub(crate) struct Publisher<T> {
    sender: mpsc::Sender<Result<T>>,
    notify: UnixStream,
    pub(crate) cancellation: CancellationToken,
}

impl<T> Publisher<T> {
    pub(crate) fn new(
        sender: mpsc::Sender<Result<T>>,
        notify: UnixStream,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            sender,
            notify,
            cancellation,
        }
    }

    pub(crate) async fn send(&mut self, endpoint: &Endpoint, value: Result<T>) -> bool {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => return false,
            _ = endpoint.wait_revoked() => return false,
            result = self.sender.send(value) => if result.is_err() { return false; },
        }
        match self.notify.write_all(&[1]) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::WouldBlock,
        }
    }
}
