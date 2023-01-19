use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::time::{sleep, Duration, Instant, Sleep};

pub struct Delay {
    inner: Pin<Box<Sleep>>,
}

impl Delay {
    pub fn new(dur: Duration) -> Self {
        Self {
            inner: Box::pin(sleep(dur)),
        }
    }

    pub fn reset(&mut self, dur: Duration) {
        self.inner.as_mut().reset(Instant::now() + dur);
    }
}

impl Future for Delay {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}
