//! Asynchronous tasks executor.

use super::{
    context,
    rand::GlobalRng,
    runtime,
    time::{TimeHandle, TimeRuntime},
    utils::mpsc,
};
use crate::assert_send_sync;
use async_task::{FallibleTask, Runnable};
use backtrace::Backtrace;
use erasable::{ErasablePtr, ErasedPtr};
use futures::pin_mut;
use rand::Rng;
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    ops::Deref,
    panic::{RefUnwindSafe, UnwindSafe},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use tracing::{error_span, info, trace, warn, Span};

pub use tokio::msim_adapter::{join_error, runtime_task};
pub use tokio::task::{yield_now, JoinError};
pub use tokio::{select, sync::watch};

pub mod join_set;
pub use join_set::JoinSet;

pub(crate) struct Executor {
    queue: mpsc::Receiver<(Runnable, Arc<TaskInfo>)>,
    handle: TaskHandle,
    rand: GlobalRng,
    time: TimeRuntime,
    time_limit: Option<Duration>,
}

/// A unique identifier for a node.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.0)
    }
}

impl NodeId {
    pub(crate) const fn zero() -> Self {
        NodeId(0)
    }
}

pub(crate) struct NodeInfo {
    pub node: NodeId,
    pub name: String,
    span: Span,
}

#[derive(Debug)]
struct PanicWrapper {
    // how long should the node stay down. If None, node does not reboot.
    restart_after: Option<Duration>,
}

struct PanicHookGuard(Option<Box<dyn Fn(&std::panic::PanicInfo<'_>) + Sync + Send + 'static>>);

impl PanicHookGuard {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn call_hook(&self, info: &std::panic::PanicInfo<'_>) {
        self.0.as_ref().unwrap()(info);
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::panic::set_hook(self.0.take().unwrap());
        }
    }
}

/// Kill the current node by panicking with a special type that tells the executor to kill the
/// current node instead of terminating the test.
pub fn kill_current_node(restart_after: Option<Duration>) {
    let handle = runtime::Handle::current();
    let restart_after = restart_after.unwrap_or_else(|| {
        Duration::from_millis(handle.rand.with(|rng| rng.gen_range(1000..3000)))
    });
    kill_current_node_impl(handle, Some(restart_after));
}

/// Kill the current node, and do not restart it automatically.
pub fn shutdown_current_node() {
    kill_current_node_impl(runtime::Handle::current(), None);
}

fn kill_current_node_impl(handle: runtime::Handle, restart_after: Option<Duration>) {
    let cur_node_id = context::current_node();

    if let Some(restart_after) = restart_after {
        info!(
            "killing node {}. Will restart in {:?}",
            cur_node_id, restart_after
        );
    } else {
        info!("shutting down node {}", cur_node_id);
    }
    handle.kill(cur_node_id);
    // panic with PanicWrapper so that run_all_ready can intercept it.
    std::panic::panic_any(PanicWrapper { restart_after });
}

pub(crate) struct TaskInfo {
    inner: Arc<NodeInfo>,
    /// A flag indicating that the task should be paused.
    paused: AtomicBool,
    /// A flag indicating that the task should no longer be executed.
    killed: watch::Sender<bool>,
}

impl TaskInfo {
    fn new(node_id: NodeId, name: String) -> Self {
        let span = error_span!(parent: None, "node", id = %node_id.0, name);
        TaskInfo {
            inner: Arc::new(NodeInfo {
                node: node_id,
                name,
                span,
            }),
            paused: AtomicBool::new(false),
            killed: watch::channel(false).0,
        }
    }

    pub fn node(&self) -> NodeId {
        self.inner.node
    }

    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    pub fn span(&self) -> Span {
        self.inner.span.clone()
    }

    pub fn is_killed(&self) -> bool {
        *self.killed.borrow()
    }
}

// Instrumentation to capture the stack trace of points of interest in the code.
// Code can call `instrumented_yield` to allow the scheduler to see the stack trace
// of the task that just yielded.
thread_local! {
    static LAST_CAPTURE: Mutex<Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>> = Mutex::new(None);
}

#[derive(Default)]
struct YieldToScheduler(Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>);

impl Future for YieldToScheduler {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // capture the current stack trace
        LAST_CAPTURE.with(|last_capture| {
            let mut last_capture = last_capture.lock().unwrap();
            match (&self.0, last_capture.as_ref()) {
                (Some(this), Some(last)) => {
                    assert!(Arc::ptr_eq(this, last));
                    // we were polled again before control reached the scheduler
                    warn!("YieldToScheduler polled before being woken");
                    Poll::Pending
                }
                (Some(_), None) => {
                    // the scheduler cleared the capture, so we are ready to resume.
                    Poll::Ready(())
                }
                (None, Some(_)) => {
                    // If this happens and can't be avoided, we could keep a Vec instead of Option
                    // in LAST_CAPTURE, although the scheduler will have no ability to change the
                    // ordering of such events.
                    panic!(
                        "instrumented_yield() called twice before control returned to scheduler"
                    );
                }
                (None, None) => {
                    trace!("capturing stack trace and yielding");
                    let task = context::current_task();
                    let new_capture = Arc::new((task, cx.waker().clone(), Backtrace::new()));
                    *last_capture = Some(new_capture.clone());
                    self.0 = Some(new_capture);
                    Poll::Pending
                }
            }
        })
    }
}

/// Capture the current stack trace and attempt to yield execution back to the scheduler.
///
/// Note that it is not possible to guarantee that execution immediately returns all the way to
/// the scheduler, as the yielding future may be wrapped in another future that polls other futures
/// (for instance a `select!` macro, or FuturesOrdered/FuturesUnordered collection). Also, it is
/// possible for an intermediate future to poll() the YieldToScheduler instance again before it is
/// woken.  However, we do guarantee not to wake the future until execution has returned to the
/// scheduler.
pub fn instrumented_yield() -> Box<dyn Future<Output = ()>> {
    Box::new(YieldToScheduler::default())
}

fn take_last_capture() -> Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>> {
    LAST_CAPTURE.with(|last_capture| last_capture.lock().unwrap().take())
}

impl Executor {
    pub fn new(rand: GlobalRng) -> Self {
        let (sender, queue) = mpsc::channel();
        Executor {
            queue,
            handle: TaskHandle {
                nodes: Arc::new(Mutex::new(HashMap::new())),
                sender,
                next_node_id: Arc::new(AtomicU64::new(1)),
            },
            time: TimeRuntime::new(&rand),
            rand,
            time_limit: None,
        }
    }

    pub fn handle(&self) -> &TaskHandle {
        &self.handle
    }

    pub fn time_handle(&self) -> &TimeHandle {
        self.time.handle()
    }

    pub fn set_time_limit(&mut self, limit: Duration) {
        self.time_limit = Some(limit);
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let mut task = self.spawn_on_main_task(future);

        // empty context to poll the result
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        loop {
            self.run_all_ready();
            if let Poll::Ready(val) = Pin::new(&mut task).poll(&mut cx) {
                return val;
            }
            let going = self.time.advance_to_next_event();
            assert!(going, "no events, the task will block forever");
            if let Some(limit) = self.time_limit {
                assert!(
                    self.time.handle().elapsed() < limit,
                    "time limit exceeded: {:?}",
                    limit
                )
            }
        }
    }

    fn spawn_on_main_task<F: Future>(&self, future: F) -> async_task::Task<F::Output> {
        let sender = self.handle.sender.clone();
        let info = Arc::new(TaskInfo::new(NodeId(0), "main".into()));
        let (runnable, task) = unsafe {
            // Safety: The schedule is not Sync,
            // the task's Waker must be used and dropped on the original thread.
            async_task::spawn_unchecked(future, move |runnable| {
                sender.send((runnable, info.clone())).unwrap();
            })
        };
        runnable.schedule();
        task
    }

    /// Drain all tasks from ready queue and run them.
    fn run_all_ready(&self) {
        let hook_guard = Arc::new(PanicHookGuard::new());
        let hook_guard_clone = Arc::downgrade(&hook_guard);
        std::panic::set_hook(Box::new(move |panic_info| {
            if panic_info
                .payload()
                .downcast_ref::<PanicWrapper>()
                .is_none()
            {
                if let Some(old_hook) = hook_guard_clone.upgrade() {
                    old_hook.call_hook(panic_info);
                }
            }
        }));

        while let Ok((runnable, info)) = self.queue.try_recv_random(&self.rand) {
            if *info.killed.borrow() {
                // killed task: must enter the task before dropping it, so that
                // Drop impls can run.
                let _guard = crate::context::enter_task(info);
                std::mem::drop(runnable);
                continue;
            } else if info.paused.load(Ordering::SeqCst) {
                // paused task: push to waiting list
                let mut nodes = self.nodes.lock().unwrap();
                nodes.get_mut(&info.node()).unwrap().paused.push(runnable);
                continue;
            }
            // run task
            let node_id = info.node();
            let _guard = crate::context::enter_task(info);
            let panic_guard = PanicGuard(self);

            let result = std::panic::catch_unwind(|| {
                runnable.run();
            });

            if let Some(capture) = take_last_capture() {
                let (_task, waker, _captured_stack) = &*capture;
                waker.wake_by_ref();

                // Examine stack trace of previously yielded task
            }

            if let Err(err) = result {
                if let Some(panic_info) = err.downcast_ref::<PanicWrapper>() {
                    if let Some(restart_after) = panic_info.restart_after {
                        let task = self.spawn_on_main_task(async move {
                            crate::time::sleep(restart_after).await;
                            info!("restarting node {}", node_id);
                            runtime::Handle::current().restart(node_id);
                        });

                        task.fallible().detach();
                    }
                } else {
                    std::panic::resume_unwind(err);
                }
            }

            // panic guard only runs if runnable.run() panics - in that case
            // we must drop all tasks before exiting the task, since they may have Drop impls that
            // assume access to the current task/runtime.
            std::mem::forget(panic_guard);

            // advance time: 50-100ns
            let dur = Duration::from_nanos(self.rand.with(|rng| rng.gen_range(50..100)));
            self.time.advance(dur);
        }
    }
}

struct PanicGuard<'a>(&'a Executor);
impl<'a> Drop for PanicGuard<'a> {
    fn drop(&mut self) {
        trace!("panic detected - dropping all tasks immediately");
        self.0.queue.clear_inner();
    }
}

impl Deref for Executor {
    type Target = TaskHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

#[derive(Clone)]
pub(crate) struct TaskHandle {
    sender: mpsc::Sender<(Runnable, Arc<TaskInfo>)>,
    nodes: Arc<Mutex<HashMap<NodeId, Node>>>,
    next_node_id: Arc<AtomicU64>,
}
assert_send_sync!(TaskHandle);

struct Node {
    info: Arc<TaskInfo>,
    paused: Vec<Runnable>,
    /// A function to spawn the initial task.
    init: Option<Arc<dyn Fn(&TaskNodeHandle) + Send + Sync>>,
}

impl TaskHandle {
    /// Kill all tasks of the node.
    pub fn kill(&self, id: NodeId) {
        TimeHandle::current().disable_node_and_cancel_timers(id);

        let mut nodes = self.nodes.lock().unwrap();
        let node = nodes.get_mut(&id).expect("node not found");
        node.paused.clear();
        let new_info = Arc::new(TaskInfo::new(id, node.info.name()));
        let old_info = std::mem::replace(&mut node.info, new_info);
        old_info.killed.send_replace(true);
    }

    /// Kill all tasks of the node and restart the initial task.
    pub fn restart(&self, id: NodeId) {
        self.kill(id);
        TimeHandle::current().enable_node(id);

        let nodes = self.nodes.lock().unwrap();
        let node = nodes.get(&id).expect("node not found");
        if let Some(init) = &node.init {
            init(&TaskNodeHandle {
                sender: self.sender.clone(),
                info: node.info.clone(),
            });
        }
    }

    /// Pause all tasks of the node.
    pub fn pause(&self, id: NodeId) {
        let nodes = self.nodes.lock().unwrap();
        let node = nodes.get(&id).expect("node not found");
        node.info.paused.store(true, Ordering::SeqCst);
    }

    /// Resume the execution of the address.
    pub fn resume(&self, id: NodeId) {
        let mut nodes = self.nodes.lock().unwrap();
        let node = nodes.get_mut(&id).expect("node not found");
        node.info.paused.store(false, Ordering::SeqCst);

        // take paused tasks from waiting list and push them to ready queue
        for runnable in node.paused.drain(..) {
            self.sender.send((runnable, node.info.clone())).unwrap();
        }
    }

    /// Create a new node.
    pub fn create_node(
        &self,
        name: Option<String>,
        init: Option<Arc<dyn Fn(&TaskNodeHandle) + Send + Sync>>,
    ) -> TaskNodeHandle {
        let id = NodeId(self.next_node_id.fetch_add(1, Ordering::SeqCst));
        let name = name.unwrap_or_else(|| format!("node-{}", id.0));
        let info = Arc::new(TaskInfo::new(id, name));
        let handle = TaskNodeHandle {
            sender: self.sender.clone(),
            info: info.clone(),
        };
        if let Some(init) = &init {
            init(&handle);
        }
        let node = Node {
            info,
            paused: vec![],
            init,
        };
        self.nodes.lock().unwrap().insert(id, node);
        handle
    }

    pub fn delete_node(&self, id: NodeId) {
        self.kill(id);
        let mut nodes = self.nodes.lock().unwrap();
        assert!(nodes.remove(&id).is_some());
    }

    /// Get the node handle.
    pub fn get_node(&self, id: NodeId) -> Option<TaskNodeHandle> {
        let nodes = self.nodes.lock().unwrap();
        let info = nodes.get(&id)?.info.clone();
        Some(TaskNodeHandle {
            sender: self.sender.clone(),
            info,
        })
    }
}

#[derive(Clone)]
pub(crate) struct TaskNodeHandle {
    sender: mpsc::Sender<(Runnable, Arc<TaskInfo>)>,
    info: Arc<TaskInfo>,
}

assert_send_sync!(TaskNodeHandle);

impl TaskNodeHandle {
    pub fn current() -> Self {
        Self::try_current().unwrap()
    }

    pub fn try_current() -> Option<Self> {
        let info = crate::context::try_current_task()?;
        let sender = crate::context::try_current(|h| h.task.sender.clone())?;
        Some(TaskNodeHandle { sender, info })
    }

    pub(crate) fn id(&self) -> NodeId {
        self.info.node()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_local(future)
    }

    pub fn spawn_local<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let sender = self.sender.clone();
        let info = self.info.clone();
        let mut killed_rx = info.killed.subscribe();

        let future = async move {
            pin_mut!(future);
            loop {
                select! {
                    _ = killed_rx.changed() => {
                        if *killed_rx.borrow() {
                            // when a cancelled task is run by run_all_ready(), it is dropped rather
                            // than being executed. Therefore this should never run. However, we must
                            // poll killed_rx in order to force this task to wake up when its node is
                            // killed. (Otherwise the task will not be dropped until its next
                            // scheduled wakeup, which may be never if it is listening for network
                            // messages).
                            panic!("killed task must not run!");
                        }
                    }

                    output = &mut future => {
                        break output;
                    }
                }
            }
        };

        let (runnable, task) = unsafe {
            // Safety: The schedule is not Sync,
            // the task's Waker must be used and dropped on the original thread.
            async_task::spawn_unchecked(future, move |runnable| {
                let _ = sender.send((runnable, info.clone()));
            })
        };
        runnable.schedule();

        JoinHandle {
            id: runtime_task::next_task_id(),
            inner: Arc::new(InnerHandle::new(Mutex::new(Some(task.fallible())))),
        }
    }

    pub fn enter(&self) -> crate::context::TaskEnterGuard {
        crate::context::enter_task(self.info.clone())
    }

    pub async fn await_future_in_node<F: Future>(&self, fut: F) -> F::Output {
        let wrapped = TaskEnteringFuture::new(self.info.clone(), fut);
        wrapped.await
    }
}

// Polls a wrapped future, entering the given task before each poll().
struct TaskEnteringFuture<F: Future> {
    task: Arc<TaskInfo>,
    inner: Pin<Box<F>>,
}

impl<F: Future> TaskEnteringFuture<F> {
    fn new(task: Arc<TaskInfo>, inner: F) -> Self {
        Self {
            task,
            inner: Box::pin(inner),
        }
    }
}

impl<F> Future for TaskEnteringFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = crate::context::enter_task(self.task.clone());
        self.inner.as_mut().poll(cx)
    }
}

/// Spawns a new asynchronous task, returning a [`JoinHandle`] for it.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = TaskNodeHandle::current();
    handle.spawn(future)
}

/// Spawns a `!Send` future on the local task set.
pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let handle = TaskNodeHandle::current();
    handle.spawn_local(future)
}

/// Runs the provided closure on a thread where blocking is acceptable.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let handle = TaskNodeHandle::current();
    handle.spawn(async move { f() })
}

#[derive(Debug)]
struct InnerHandle<T> {
    task: Mutex<Option<FallibleTask<T>>>,
}

impl<T> InnerHandle<T> {
    fn new(task: Mutex<Option<FallibleTask<T>>>) -> Self {
        Self { task }
    }

    // Important: Because InnerHandle is type erased and then un-erased with T = (),
    // we can't call any on methods on FallibleTask that deal with T. The drop and is_finished
    // methods only access the task header pointer, and don't depend on the type.
    // TODO: this really needs support from async-task for abort handles.
    fn abort(&self) {
        self.task.lock().unwrap().take();
    }

    fn is_finished(&self) -> bool {
        self.task
            .lock()
            .unwrap()
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(true)
    }
}

/// An owned permission to join on a task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    id: runtime_task::Id,
    inner: Arc<InnerHandle<T>>,
}

impl<T> JoinHandle<T> {
    /// Abort the task associated with the handle.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Check if the task associate with the handle is finished.
    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// Cancel the task when this handle is dropped.
    #[doc(hidden)]
    pub fn cancel_on_drop(self) -> FallibleTask<T> {
        self.inner.task.lock().unwrap().take().unwrap()
    }

    /// Return an AbortHandle corresponding for the task.
    pub fn abort_handle(&self) -> AbortHandle {
        let inner = ErasablePtr::erase(Box::new(self.inner.clone()));
        let id = self.id.clone();
        AbortHandle { id, inner }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut lock = self.inner.task.lock().unwrap();
        let task = lock.as_mut();
        if task.is_none() {
            return std::task::Poll::Ready(Err(join_error::cancelled(self.id.clone())));
        }
        std::pin::Pin::new(task.unwrap()).poll(cx).map(|res| {
            // TODO: decide cancelled or panic
            res.ok_or(join_error::cancelled(self.id.clone()))
        })
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.inner.task.lock().unwrap().take() {
            task.detach();
        }
    }
}

/// AbortHandle allows aborting, but not awaiting the return value.
pub struct AbortHandle {
    id: runtime_task::Id,
    inner: ErasedPtr,
}

unsafe impl Send for AbortHandle {}
unsafe impl Sync for AbortHandle {}

impl AbortHandle {
    /// abort the task
    pub fn abort(&self) {
        let inner = self.inner();
        inner.abort();
        std::mem::forget(inner);
    }

    /// Check if the task associate with the handle is finished.
    pub fn is_finished(&self) -> bool {
        let inner = self.inner();
        let ret = inner.is_finished();
        std::mem::forget(inner);
        ret
    }

    fn inner(&self) -> Box<Arc<InnerHandle<()>>> {
        unsafe { ErasablePtr::unerase(self.inner) }
    }
}

impl Drop for AbortHandle {
    fn drop(&mut self) {
        // must turn our erased pointer back into a Box and drop it.
        let inner = self.inner();
        std::mem::drop(inner);
    }
}

impl UnwindSafe for AbortHandle {}
impl RefUnwindSafe for AbortHandle {}

impl fmt::Debug for AbortHandle {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("AbortHandle")
            .field("id", &self.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{Handle, NodeHandle, Runtime},
        time,
    };
    use join_set::JoinSet;
    use std::{collections::HashSet, sync::atomic::AtomicUsize, time::Duration};

    #[test]
    fn spawn_in_block_on() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            spawn(async { 1 }).await.unwrap();
            spawn_local(async { 2 }).await.unwrap();
        });
    }

    #[test]
    fn kill() {
        let runtime = Runtime::new();
        let node1 = runtime.create_node().build();
        let node2 = runtime.create_node().build();

        let flag1 = Arc::new(AtomicUsize::new(0));
        let flag2 = Arc::new(AtomicUsize::new(0));

        let flag1_ = flag1.clone();
        node1.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag1_.fetch_add(2, Ordering::SeqCst);
            }
        });

        let flag2_ = flag2.clone();
        node2.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag2_.fetch_add(2, Ordering::SeqCst);
            }
        });

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag1.load(Ordering::SeqCst), 2);
            assert_eq!(flag2.load(Ordering::SeqCst), 2);
            Handle::current().kill(node1.id());
            Handle::current().kill(node1.id());

            time::sleep_until(t0 + Duration::from_secs(5)).await;
            assert_eq!(flag1.load(Ordering::SeqCst), 2);
            assert_eq!(flag2.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn restart() {
        let runtime = Runtime::new();

        let flag = Arc::new(AtomicUsize::new(0));

        let flag_ = flag.clone();
        let node = runtime
            .create_node()
            .init(move || {
                let flag = flag_.clone();
                async move {
                    // set flag to 0, then +2 every 2s
                    flag.store(0, Ordering::SeqCst);
                    loop {
                        time::sleep(Duration::from_secs(2)).await;
                        flag.fetch_add(2, Ordering::SeqCst);
                    }
                }
            })
            .build();

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);
            Handle::current().kill(node.id());
            Handle::current().restart(node.id());

            time::sleep_until(t0 + Duration::from_secs(6)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);

            time::sleep_until(t0 + Duration::from_secs(8)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn pause_resume() {
        let runtime = Runtime::new();
        let node = runtime.create_node().build();

        let flag = Arc::new(AtomicUsize::new(0));
        let flag_ = flag.clone();
        node.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag_.fetch_add(2, Ordering::SeqCst);
            }
        });

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);
            Handle::current().pause(node.id());
            Handle::current().pause(node.id());

            time::sleep_until(t0 + Duration::from_secs(5)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);

            Handle::current().resume(node.id());
            Handle::current().resume(node.id());
            time::sleep_until(t0 + Duration::from_secs_f32(5.5)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn random_select_from_ready_tasks() {
        let mut seqs = HashSet::new();
        for seed in 0..10 {
            let runtime = Runtime::with_seed_and_config(seed, crate::SimConfig::default());
            let seq = runtime.block_on(async {
                let (tx, rx) = std::sync::mpsc::channel();
                let mut tasks = vec![];
                for i in 0..3 {
                    let tx = tx.clone();
                    tasks.push(spawn(async move {
                        for j in 0..5 {
                            tx.send(i * 10 + j).unwrap();
                            tokio::task::yield_now().await;
                        }
                    }));
                }
                drop(tx);
                futures::future::join_all(tasks).await;
                rx.into_iter().collect::<Vec<_>>()
            });
            seqs.insert(seq);
        }
        assert_eq!(seqs.len(), 10);
    }

    #[test]
    fn await_future_in_node() {
        let runtime = Runtime::new();
        let node1 = runtime.create_node().build();
        let node2 = runtime.create_node().build();
        let node1_id = node1.id();

        runtime.block_on(async move {
            node1
                .spawn(async move {
                    let id = node2
                        .await_future_in_node(async move {
                            tokio::task::yield_now().await;
                            NodeHandle::current().id()
                        })
                        .await;

                    assert_eq!(id, node2.id());
                    assert_eq!(NodeHandle::current().id(), node1_id);
                })
                .await
                .unwrap();
        });
    }

    #[test]
    fn test_abort() {
        let runtime = Runtime::new();

        fn panic_int() -> i32 {
            panic!();
        }

        runtime.block_on(async move {
            let jh = spawn(async move {
                time::sleep(Duration::from_secs(5)).await;
                panic_int()
            });
            time::sleep(Duration::from_secs(1)).await;
            jh.abort();
            jh.await.unwrap_err();
        });

        runtime.block_on(async move {
            let jh = spawn(async move {
                time::sleep(Duration::from_secs(5)).await;
                panic_int()
            });
            time::sleep(Duration::from_secs(1)).await;
            let ah = jh.abort_handle();
            ah.abort();
            jh.await.unwrap_err();
        });
    }

    #[test]
    fn test_joinset() {
        let runtime = Runtime::new();

        // test joining
        runtime.block_on(async move {
            let mut join_set = JoinSet::new();

            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                3
            });
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(2)).await;
                2
            });
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(1)).await;
                1
            });

            let mut res = Vec::new();
            while let Some(next) = join_set.join_next().await {
                res.push(next.unwrap());
            }
            assert_eq!(res, vec![1, 2, 3]);
        });

        // test cancelling
        runtime.block_on(async move {
            let mut join_set = JoinSet::new();

            // test abort_all()
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                panic!();
            });
            time::sleep(Duration::from_secs(1)).await;
            join_set.abort_all();
            time::sleep(Duration::from_secs(5)).await;

            // test drop
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                panic!();
            });
            time::sleep(Duration::from_secs(1)).await;
            std::mem::drop(join_set);
            time::sleep(Duration::from_secs(5)).await;
        });

        // test detach
        runtime.block_on(async move {
            let flag = Arc::new(AtomicBool::new(false));
            let mut join_set = JoinSet::new();

            let flag1 = flag.clone();
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                flag1.store(true, Ordering::Relaxed);
            });
            time::sleep(Duration::from_secs(1)).await;
            join_set.detach_all();
            time::sleep(Duration::from_secs(5)).await;
            assert_eq!(flag.load(Ordering::Relaxed), true);
        });
    }
}
