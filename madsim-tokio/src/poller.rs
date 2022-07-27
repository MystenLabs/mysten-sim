use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

type PollerPinFut<R> = Pin<Box<dyn Future<Output = R> + Send + Sync>>;
pub struct Poller<R> {
    p: Arc<Mutex<Option<PollerPinFut<R>>>>,
}

impl<R> Poller<R> {
    pub fn new() -> Self {
        Self {
            p: Arc::new(Mutex::new(None)),
        }
    }

    pub fn poll_with_fut<F, T>(&self, cx: &mut Context<'_>, fut_fn: F) -> Poll<R>
    where
        F: FnOnce() -> T,
        T: Future<Output = R> + 'static + Send + Sync,
    {
        let mut poller = self.p.lock().unwrap();

        if poller.is_none() {
            *poller = Some(Box::pin(fut_fn()));
        }

        let fut: &mut PollerPinFut<R> = poller.as_mut().unwrap();
        let res = fut.as_mut().poll(cx);

        if let Poll::Ready(_) = res {
            poller.take();
        }
        res
    }
}
