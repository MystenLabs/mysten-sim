//! API Compatible implementation of tokio::task::JoinSet

#![allow(missing_docs)]

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::poll_fn;
use futures::stream::{FuturesUnordered, Stream};

use crate::runtime::Handle;
#[cfg(tokio_unstable)]
use crate::task::Id;
use crate::task::{AbortHandle, JoinError, JoinHandle};

use tokio::task::LocalSet;

pub struct JoinSet<T: 'static> {
    inner: FuturesUnordered<JoinHandle<T>>,
}

impl<T> JoinSet<T> {
    pub fn new() -> Self {
        Self {
            inner: FuturesUnordered::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl<T: 'static> JoinSet<T> {
    pub fn spawn<F>(&mut self, task: F) -> AbortHandle
    where
        F: Future<Output = T>,
        F: Send + 'static,
        T: Send,
    {
        self.insert(crate::task::spawn(task))
    }

    pub fn spawn_on<F>(&mut self, task: F, _handle: &Handle) -> AbortHandle
    where
        F: Future<Output = T>,
        F: Send + 'static,
        T: Send,
    {
        self.insert(crate::task::spawn(task))
    }

    pub fn spawn_local<F>(&mut self, task: F) -> AbortHandle
    where
        F: Future<Output = T>,
        F: 'static,
    {
        self.insert(crate::task::spawn_local(task))
    }

    pub fn spawn_local_on<F>(&mut self, task: F, _local_set: &LocalSet) -> AbortHandle
    where
        F: Future<Output = T>,
        F: 'static,
    {
        self.insert(crate::task::spawn_local(task))
    }

    fn insert(&mut self, jh: JoinHandle<T>) -> AbortHandle {
        let abort = jh.abort_handle();
        self.inner.push(jh);
        abort
    }

    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        poll_fn(|cx| self.poll_join_next(cx)).await
    }

    pub async fn shutdown(&mut self) {
        self.abort_all();
        while self.join_next().await.is_some() {}
    }

    pub fn abort_all(&mut self) {
        self.inner.iter().for_each(|jh| jh.abort());
    }

    pub fn detach_all(&mut self) {
        let mut new_inner = FuturesUnordered::new();
        std::mem::swap(&mut new_inner, &mut self.inner);
    }

    pub fn poll_join_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<T, JoinError>>> {
        let pinned = Pin::new(&mut self.inner);
        pinned.poll_next(cx)
    }
}

impl<T: 'static> Drop for JoinSet<T> {
    fn drop(&mut self) {
        self.inner
            .iter()
            .for_each(|join_handle| join_handle.abort());
    }
}

impl<T> fmt::Debug for JoinSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinSet").field("len", &self.len()).finish()
    }
}

impl<T> Default for JoinSet<T> {
    fn default() -> Self {
        Self::new()
    }
}
