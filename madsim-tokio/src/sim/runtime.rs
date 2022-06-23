
use std::io;
use std::future::Future;

pub struct Handle {}

pub struct Runtime {}

pub struct EnterGuard<'a> {
    _lt: std::marker::PhantomData<&'a Handle>
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct Id(u64);

pub struct JoinHandle<T> {
    _raw: (),
    id: Id,
    _p: std::marker::PhantomData<T>,
}

impl Drop for EnterGuard<'_> {
    fn drop(&mut self) {
        todo!()
    }
}

impl Runtime {
    pub fn block_on<F: Future>(&self, _future: F) -> F::Output {
        todo!()
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        //self.handle.enter()
        todo!()
    }

    pub fn spawn<F>(&self, _future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        todo!()
    }
}

pub struct Builder {}

impl Builder {
    pub fn new() -> Self {
        todo!();
    }

    pub fn new_multi_thread() -> Self {
        todo!();
    }

    pub fn enable_all(&mut self) -> &mut Self {
        todo!()
    }

    pub fn enable_io(&mut self) -> &mut Self {
        todo!()
    }

    pub fn enable_time(&mut self) -> &mut Self {
        todo!()
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        todo!()
    }
}
