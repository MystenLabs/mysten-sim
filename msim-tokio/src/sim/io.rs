pub use real_tokio::io::*;

pub mod unix {
    use futures::{future::poll_fn, ready};
    use msim::net::get_endpoint_from_socket;
    use msim::rand::{prelude::thread_rng, Rng};
    use msim::time::{Duration, Instant, TimeHandle};
    use real_tokio::io;
    use real_tokio::io::Interest;
    use std::os::unix::io::{AsRawFd, RawFd};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{task::Context, task::Poll};

    /// Reimplementation of AsyncFd for simulator.
    /// Only works with UDP sockets right now.
    pub struct AsyncFd<T: AsRawFd> {
        inner: Option<T>,
        next_ready_write_nanos: AtomicU64,
    }

    #[must_use = "You must explicitly choose whether to clear the readiness state by calling a method on ReadyGuard"]
    pub struct AsyncFdReadyGuard<'a, T: AsRawFd> {
        async_fd: &'a AsyncFd<T>,
    }

    #[must_use = "You must explicitly choose whether to clear the readiness state by calling a method on ReadyGuard"]
    pub struct AsyncFdReadyMutGuard<'a, T: AsRawFd> {
        async_fd: &'a mut AsyncFd<T>,
    }

    const ALL_INTEREST: Interest = Interest::READABLE.add(Interest::WRITABLE);

    impl<T: AsRawFd> AsyncFd<T> {
        #[inline]
        pub fn new(inner: T) -> io::Result<Self>
        where
            T: AsRawFd,
        {
            Self::with_interest(inner, ALL_INTEREST)
        }

        #[inline]
        pub fn with_interest(inner: T, interest: Interest) -> io::Result<Self>
        where
            T: AsRawFd,
        {
            Self::new_with_handle_and_interest(inner, interest)
        }

        pub(crate) fn new_with_handle_and_interest(
            inner: T,
            _interest: Interest,
        ) -> io::Result<Self> {
            Ok(AsyncFd {
                inner: Some(inner),
                next_ready_write_nanos: AtomicU64::new(0),
            })
        }

        #[inline]
        pub fn get_ref(&self) -> &T {
            self.inner.as_ref().unwrap()
        }

        #[inline]
        pub fn get_mut(&mut self) -> &mut T {
            self.inner.as_mut().unwrap()
        }

        fn take_inner(&mut self) -> Option<T> {
            self.inner.take()
        }

        pub fn into_inner(mut self) -> T {
            self.take_inner().unwrap()
        }

        pub fn poll_read_ready<'a>(
            &'a self,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<AsyncFdReadyGuard<'a, T>>> {
            ready!(self.poll_ready_ready_impl(cx))?;
            Ok(AsyncFdReadyGuard { async_fd: self }).into()
        }

        pub fn poll_read_ready_mut<'a>(
            &'a mut self,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<AsyncFdReadyMutGuard<'a, T>>> {
            ready!(self.poll_ready_ready_impl(cx))?;
            Ok(AsyncFdReadyMutGuard { async_fd: self }).into()
        }

        fn poll_ready_ready_impl(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let ep = match get_endpoint_from_socket(self.inner.as_ref().unwrap().as_raw_fd()) {
                Err(err) => return Poll::Ready(Err(err)),
                Ok(ep) => ep,
            };
            let port = ep.udp_tag();
            match port {
                Err(err) => Err(err).into(),
                Ok(port) => match ep.recv_ready(Some(cx), port) {
                    Err(err) => Err(err).into(),
                    Ok(true) => Ok(()).into(),
                    Ok(false) => Poll::Pending,
                },
            }
        }

        pub fn poll_write_ready<'a>(
            &'a self,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<AsyncFdReadyGuard<'a, T>>> {
            ready!(self.poll_write_ready_impl(cx))?;
            Ok(AsyncFdReadyGuard { async_fd: self }).into()
        }

        pub fn poll_write_ready_mut<'a>(
            &'a mut self,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<AsyncFdReadyMutGuard<'a, T>>> {
            ready!(self.poll_write_ready_impl(cx))?;
            Ok(AsyncFdReadyMutGuard { async_fd: self }).into()
        }

        fn poll_write_ready_impl(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let handle = TimeHandle::current();
            let now_nanos: u64 = handle.elapsed().as_nanos().try_into().unwrap();
            let ready = self.next_ready_write_nanos.load(Ordering::Relaxed);
            if now_nanos >= ready {
                self.next_ready_write_nanos
                    .store(now_nanos + thread_rng().gen_range(0..50), Ordering::Relaxed);

                Ok(()).into()
            } else {
                handle.wake_at(
                    Instant::now() + Duration::from_nanos(ready - now_nanos),
                    cx.waker().clone(),
                );
                Poll::Pending
            }
        }

        async fn readiness(&self, interest: Interest) -> io::Result<AsyncFdReadyGuard<'_, T>> {
            match interest {
                Interest::READABLE => poll_fn(|cx| self.poll_read_ready(cx)).await,
                Interest::WRITABLE => poll_fn(|cx| self.poll_write_ready(cx)).await,
                _ => unimplemented!("unhandled interested {:?}", interest),
            }
        }

        async fn readiness_mut(
            &mut self,
            interest: Interest,
        ) -> io::Result<AsyncFdReadyMutGuard<'_, T>> {
            let shared_self = &*self;
            match interest {
                Interest::READABLE => poll_fn(|cx| shared_self.poll_ready_ready_impl(cx)).await?,
                Interest::WRITABLE => poll_fn(|cx| shared_self.poll_write_ready_impl(cx)).await?,
                _ => unimplemented!("unhandled interested {:?}", interest),
            }

            Ok(AsyncFdReadyMutGuard { async_fd: self }).into()
        }

        #[allow(clippy::needless_lifetimes)] // The lifetime improves rustdoc rendering.
        pub async fn readable<'a>(&'a self) -> io::Result<AsyncFdReadyGuard<'a, T>> {
            self.readiness(Interest::READABLE).await
        }

        #[allow(clippy::needless_lifetimes)] // The lifetime improves rustdoc rendering.
        pub async fn readable_mut<'a>(&'a mut self) -> io::Result<AsyncFdReadyMutGuard<'a, T>> {
            self.readiness_mut(Interest::READABLE).await
        }

        #[allow(clippy::needless_lifetimes)] // The lifetime improves rustdoc rendering.
        pub async fn writable<'a>(&'a self) -> io::Result<AsyncFdReadyGuard<'a, T>> {
            self.readiness(Interest::WRITABLE).await
        }

        #[allow(clippy::needless_lifetimes)] // The lifetime improves rustdoc rendering.
        pub async fn writable_mut<'a>(&'a mut self) -> io::Result<AsyncFdReadyMutGuard<'a, T>> {
            self.readiness_mut(Interest::WRITABLE).await
        }
    }

    impl<T: AsRawFd> AsRawFd for AsyncFd<T> {
        fn as_raw_fd(&self) -> RawFd {
            self.inner.as_ref().unwrap().as_raw_fd()
        }
    }

    impl<T: std::fmt::Debug + AsRawFd> std::fmt::Debug for AsyncFd<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("AsyncFd")
                .field("inner", &self.inner)
                .finish()
        }
    }

    impl<T: AsRawFd> Drop for AsyncFd<T> {
        fn drop(&mut self) {
            let _ = self.take_inner();
        }
    }

    impl<'a, Inner: AsRawFd> AsyncFdReadyGuard<'a, Inner> {
        pub fn clear_ready(&mut self) {}

        pub fn retain_ready(&mut self) {
            // no-op
        }

        // Alias for old name in 0.x
        #[cfg_attr(docsrs, doc(alias = "with_io"))]
        pub fn try_io<R>(
            &mut self,
            f: impl FnOnce(&'a AsyncFd<Inner>) -> io::Result<R>,
        ) -> Result<io::Result<R>, TryIoError> {
            let result = f(self.async_fd);

            if let Err(e) = result.as_ref() {
                if e.kind() == io::ErrorKind::WouldBlock {
                    self.clear_ready();
                }
            }

            match result {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => Err(TryIoError(())),
                result => Ok(result),
            }
        }

        pub fn get_ref(&self) -> &'a AsyncFd<Inner> {
            self.async_fd
        }

        pub fn get_inner(&self) -> &'a Inner {
            self.get_ref().get_ref()
        }
    }

    impl<'a, Inner: AsRawFd> AsyncFdReadyMutGuard<'a, Inner> {
        pub fn clear_ready(&mut self) {}

        pub fn retain_ready(&mut self) {
            // no-op
        }

        pub fn try_io<R>(
            &mut self,
            f: impl FnOnce(&mut AsyncFd<Inner>) -> io::Result<R>,
        ) -> Result<io::Result<R>, TryIoError> {
            let result = f(self.async_fd);

            if let Err(e) = result.as_ref() {
                if e.kind() == io::ErrorKind::WouldBlock {
                    self.clear_ready();
                }
            }

            match result {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => Err(TryIoError(())),
                result => Ok(result),
            }
        }

        pub fn get_ref(&self) -> &AsyncFd<Inner> {
            self.async_fd
        }

        pub fn get_mut(&mut self) -> &mut AsyncFd<Inner> {
            self.async_fd
        }

        pub fn get_inner(&self) -> &Inner {
            self.get_ref().get_ref()
        }

        pub fn get_inner_mut(&mut self) -> &mut Inner {
            self.get_mut().get_mut()
        }
    }

    impl<'a, T: std::fmt::Debug + AsRawFd> std::fmt::Debug for AsyncFdReadyGuard<'a, T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ReadyGuard")
                .field("async_fd", &self.async_fd)
                .finish()
        }
    }

    impl<'a, T: std::fmt::Debug + AsRawFd> std::fmt::Debug for AsyncFdReadyMutGuard<'a, T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MutReadyGuard")
                .field("async_fd", &self.async_fd)
                .finish()
        }
    }

    #[derive(Debug)]
    pub struct TryIoError(());
}
