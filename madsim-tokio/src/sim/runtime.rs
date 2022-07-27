use std::future::Future;
use std::io;

use madsim::runtime as ms_runtime;
use madsim::task as ms_task;

pub struct Handle {}

pub struct TryCurrentError;

impl Handle {
    pub fn try_current() -> Result<Self, TryCurrentError> {
        todo!();
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        JoinHandle(ms_runtime::Handle::current_node().spawn(future))
    }
}

pub struct EnterGuard<'a>(ms_runtime::EnterGuard, std::marker::PhantomData<&'a Handle>);

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct Id(u64);

pub struct JoinHandle<T>(ms_task::JoinHandle<T>);

impl Drop for EnterGuard<'_> {
    fn drop(&mut self) {
        todo!()
    }
}

pub struct Runtime {}

impl Runtime {
    fn new() -> Self {
        Self {}
    }

    pub fn block_on<F: Future>(&self, _future: F) -> F::Output {
        // there may not be a good way to do this that doesn't deadlock the sim.
        todo!()
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        // all tokio runtimes share the simulator runtime so there is nothing to enter.
        EnterGuard(
            ms_runtime::Handle::current().enter(),
            std::marker::PhantomData,
        )
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        JoinHandle(ms_runtime::Handle::current_node().spawn(future))
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
