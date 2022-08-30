use std::fmt;
use std::future::Future;
use std::io;
use std::time::Duration;

use madsim::runtime as ms_runtime;
use madsim::task::JoinHandle;

use tracing::debug;

#[derive(Clone)]
pub struct Handle {
    #[allow(dead_code)]
    inner: ms_runtime::Handle,
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle()")
    }
}

pub struct TryCurrentError;

impl Handle {
    pub fn enter(&self) -> EnterGuard<'_> {
        EnterGuard(
            ms_runtime::Handle::current().enter(),
            std::marker::PhantomData,
        )
    }

    pub fn try_current() -> Result<Self, TryCurrentError> {
        // TODO: don't panic
        Ok(Self::current())
    }

    pub fn current() -> Self {
        Self {
            inner: ms_runtime::Handle::current(),
        }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        ms_runtime::NodeHandle::current().spawn(future)
    }
}

pub struct EnterGuard<'a>(ms_runtime::EnterGuard, std::marker::PhantomData<&'a Handle>);

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct Id(u64);

pub struct Runtime {
    handle: Handle,
}

impl Runtime {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            handle: Handle {
                inner: ms_runtime::Handle::current(),
            },
        })
    }

    pub fn block_on<F: Future>(&self, _future: F) -> F::Output {
        // there may not be a good way to do this that doesn't deadlock the sim.
        todo!()
    }

    pub fn enter(&self) -> EnterGuard<'_> {
        self.handle.enter()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        ms_runtime::NodeHandle::current().spawn(future)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        debug!("todo: Drop for Runtime");
    }
}

/// Copied from the real tokio builder
pub struct Builder {
    kind: Kind,
    enable_io: bool,
    enable_time: bool,
    start_paused: bool,
    worker_threads: Option<usize>,
    max_blocking_threads: usize,
    pub(super) thread_name: ThreadNameFn,
    pub(super) thread_stack_size: Option<usize>,
    pub(super) after_start: Option<Callback>,
    pub(super) before_stop: Option<Callback>,
    pub(super) before_park: Option<Callback>,
    pub(super) after_unpark: Option<Callback>,
    pub(super) keep_alive: Option<Duration>,

    // These values are ignored - we use a custom executor. They are retained for API
    // compatibility.
    pub(super) global_queue_interval: u32,
    pub(super) event_interval: u32,
}

pub(crate) type ThreadNameFn = std::sync::Arc<dyn Fn() -> String + Send + Sync + 'static>;

pub(crate) enum Kind {
    CurrentThread,
    MultiThread,
}

type Callback = std::sync::Arc<dyn Fn() + Send + Sync>;

impl Builder {
    pub fn new_current_thread() -> Builder {
        Builder::new(Kind::CurrentThread)
    }

    pub fn new_multi_thread() -> Builder {
        Builder::new(Kind::MultiThread)
    }

    pub(crate) fn new(kind: Kind) -> Builder {
        Builder {
            kind,

            // I/O defaults to "off"
            enable_io: false,

            // Time defaults to "off"
            enable_time: false,

            // The clock starts not-paused
            start_paused: false,

            // Default to lazy auto-detection (one thread per CPU core)
            worker_threads: None,

            max_blocking_threads: 512,

            // Default thread name
            thread_name: std::sync::Arc::new(|| "tokio-runtime-worker".into()),

            // Do not set a stack size by default
            thread_stack_size: None,

            // No worker thread callbacks
            after_start: None,
            before_stop: None,
            before_park: None,
            after_unpark: None,

            keep_alive: None,

            // Defaults for these values depend on the scheduler kind, so we get them
            // as parameters.
            global_queue_interval: 1,
            event_interval: 1,
        }
    }

    pub fn enable_all(&mut self) -> &mut Self {
        self.enable_io();
        self.enable_time();

        self
    }

    pub fn worker_threads(&mut self, val: usize) -> &mut Self {
        assert!(val > 0, "Worker threads cannot be set to 0");
        self.worker_threads = Some(val);
        self
    }

    pub fn max_blocking_threads(&mut self, val: usize) -> &mut Self {
        assert!(val > 0, "Max blocking threads cannot be set to 0");
        self.max_blocking_threads = val;
        self
    }

    pub fn thread_name(&mut self, val: impl Into<String>) -> &mut Self {
        let val = val.into();
        self.thread_name = std::sync::Arc::new(move || val.clone());
        self
    }

    pub fn thread_name_fn<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        self.thread_name = std::sync::Arc::new(f);
        self
    }

    pub fn thread_stack_size(&mut self, val: usize) -> &mut Self {
        self.thread_stack_size = Some(val);
        self
    }

    pub fn on_thread_start<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.after_start = Some(std::sync::Arc::new(f));
        self
    }

    pub fn on_thread_stop<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.before_stop = Some(std::sync::Arc::new(f));
        self
    }

    pub fn on_thread_park<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.before_park = Some(std::sync::Arc::new(f));
        self
    }

    pub fn on_thread_unpark<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.after_unpark = Some(std::sync::Arc::new(f));
        self
    }

    pub fn build(&mut self) -> io::Result<Runtime> {
        match &self.kind {
            Kind::CurrentThread => self.build_basic_runtime(),
            Kind::MultiThread => self.build_threaded_runtime(),
        }
    }

    pub fn thread_keep_alive(&mut self, duration: Duration) -> &mut Self {
        self.keep_alive = Some(duration);
        self
    }

    pub fn global_queue_interval(&mut self, val: u32) -> &mut Self {
        self.global_queue_interval = val;
        self
    }

    pub fn event_interval(&mut self, val: u32) -> &mut Self {
        self.event_interval = val;
        self
    }

    fn build_basic_runtime(&mut self) -> io::Result<Runtime> {
        Runtime::new()
    }

    pub fn enable_io(&mut self) -> &mut Self {
        self.enable_io = true;
        self
    }

    pub fn enable_time(&mut self) -> &mut Self {
        self.enable_time = true;
        self
    }

    pub fn start_paused(&mut self, start_paused: bool) -> &mut Self {
        self.start_paused = start_paused;
        self
    }

    fn build_threaded_runtime(&mut self) -> io::Result<Runtime> {
        // the multi-threaded runtime is a lie. As long as no code looks at the current thread id
        // and tries to fail if it never sees more than one thread, this can't be detected. (And
        // even then, there's no guarantee that your tasks will run on multiple threads even if
        // there are actually multiple threads)
        Runtime::new()
    }
}

impl fmt::Debug for Builder {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("Builder")
            .field("worker_threads", &self.worker_threads)
            .field("max_blocking_threads", &self.max_blocking_threads)
            .field(
                "thread_name",
                &"<dyn Fn() -> String + Send + Sync + 'static>",
            )
            .field("thread_stack_size", &self.thread_stack_size)
            .field("after_start", &self.after_start.as_ref().map(|_| "..."))
            .field("before_stop", &self.before_stop.as_ref().map(|_| "..."))
            .field("before_park", &self.before_park.as_ref().map(|_| "..."))
            .field("after_unpark", &self.after_unpark.as_ref().map(|_| "..."))
            .finish()
    }
}
