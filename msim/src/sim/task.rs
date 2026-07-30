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
    task::{Context, Poll},
    time::Duration,
};

use tracing::{error_span, info, trace, Span};

pub use tokio::msim_adapter::runtime_task::Id;
pub use tokio::msim_adapter::{join_error, runtime_task};
use tokio::sync::oneshot;
pub use tokio::task::coop;
pub use tokio::task::{yield_now, JoinError};
pub use tokio::{select, sync::watch};

pub mod join_set;
pub use join_set::JoinSet;

// # Blocking-pool design sketch
//
// The simulator gains limited multi-threading: the runtime spawns a fixed number of
// extra "blocking pool" threads for code that can yield execution to other threads.
// Normally no code needs to yield - we poll() ready futures one at a time and they
// always make progress immediately, because there is no other thread that could be
// holding a lock they require. `spawn_blocking()` closures, however, run on real OS
// threads and may block; a `ThreadQuantum` serializes everything so that at most one
// thread runs at a time, and the threads are woken in a deterministic order.
//
// `spawn_blocking()` sends tasks to a blocking_queue. The run loop enforces that only
// one thing happens at a time, and wakes the blocking threads in deterministic order:
//
//     // main run loop
//     pub fn block_on<F: Future>(&self, future: F) -> F::Output {
//         let mut task = self.spawn_on_main_task(future);
//         let waker = futures::task::noop_waker();
//         let mut cx = Context::from_waker(&waker);
//         loop {
//             self.run_all_ready();
//             if let Poll::Ready(val) = Pin::new(&mut task).poll(&mut cx) {
//                 return val;
//             }
//             self.quantum.wake_all();
//             let going = self.time.advance_to_next_event();
//             assert!(going, "no events, the task will block forever");
//         }
//     }
//
//     // blocking pool thread loop (a fixed number are created at startup)
//     run_blocking_thread(thread_id: u32) {
//         self.quantum.start(thread_id);
//         loop {
//             // Blocking threads yield before starting each task; tasks themselves can
//             // also explicitly yield, generally while waiting on a notification. Rather
//             // than mocking every tokio channel, a special-purpose one calls:
//             //   #[cfg(msim)] ThreadQuantum::with(|q| q.yield());
//             self.quantum.yield();
//             if let Ok(callable) = self.blocking_queue.recv_random(&self.rand) {
//                 callable();
//             }
//         }
//     }
//
//     struct ThreadQuantum {
//         max_thread: u32,
//         active_thread: Mutex<Option<u32>>,
//         cv: CondVar,
//     }
//
//     impl ThreadQuantum {
//         // a single static ThreadQuantum lets threads call yield() from anywhere
//         fn with(cb: Fn(&ThreadQuantum));
//
//         fn start(&self, thread_id: u32) {
//             *self.active_thread.lock() = Some(thread_id);
//         }
//
//         fn yield(&self) {
//             let l = self.active_thread.lock();
//             let cur_thread = l.expect("current thread should be set");
//             *l = None;
//             // wait until we are woken
//             self.cv.wait_while(l, |active| active != Some(cur_thread));
//         }
//
//         fn wake_all(&self) {
//             for i in 0..self.max_thread {
//                 let l = self.active_thread.lock();
//                 *l = Some(i);
//                 self.cv.notify_all();                   // only the assigned thread wakes
//                 self.cv.wait_while(l, |a| a.is_some()); // wait until it calls yield()
//             }
//         }
//     }
//
// This is the original design sketch. The implementation refines several details - a
// `Turn` enum instead of `Option<u32>` so `kill()` can request a deferred abort at a
// yield point, routing `kill_current_node()` panics/restarts back to the main loop,
// per-thread condvars, lazy thread startup, and joining the pool on shutdown - but the
// core idea (one thread runs at a time, woken in deterministic order) is unchanged.
pub(crate) mod blocking;
pub use blocking::yield_blocking;
use blocking::BlockingPool;

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

struct PanicHookGuard(Option<Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>>);

impl PanicHookGuard {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn call_hook(&self, info: &std::panic::PanicHookInfo<'_>) {
        (*self.0.as_ref().unwrap())(info);
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::panic::set_hook(self.0.take().unwrap());
        }
    }
}

/// Shut down all nodes in the simulator.
pub fn shutdown_all_nodes() {
    let cur_node_id = context::current_node();
    let handle = runtime::Handle::current();
    let node_ids: Vec<_> = handle.task.nodes.lock().unwrap().keys().copied().collect();
    for node_id in node_ids {
        if node_id == cur_node_id {
            continue;
        }
        handle.kill(node_id);
    }
}

/// Kill the current node by panicking with a special type that tells the executor to kill the
/// current node instead of terminating the test.
pub fn kill_current_node(restart_after: Option<Duration>) -> ! {
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

fn kill_current_node_impl(handle: runtime::Handle, restart_after: Option<Duration>) -> ! {
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
    std::panic::panic_any(PanicWrapper { restart_after })
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

impl Executor {
    pub fn new(rand: GlobalRng) -> Self {
        let (sender, queue) = mpsc::channel();
        let blocking = Arc::new(BlockingPool::new(rand.clone()));
        Executor {
            queue,
            handle: TaskHandle {
                nodes: Arc::new(Mutex::new(HashMap::new())),
                sender,
                next_node_id: Arc::new(AtomicU64::new(1)),
                blocking,
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

        // Number of consecutive rounds in which parked blocking tasks were the only
        // remaining activity and produced no observable progress. A parked task may
        // make progress purely by being re-woken (e.g. code that yields N times), so we
        // cannot declare deadlock after one quiet round; a task genuinely waiting for a
        // condition nothing can set will stall indefinitely, which we detect after a
        // generous bound.
        let mut stalled_rounds: usize = 0;
        const MAX_STALLED_ROUNDS: usize = 10_000;

        loop {
            self.run_all_ready();
            if let Poll::Ready(val) = Pin::new(&mut task).poll(&mut cx) {
                return val;
            }

            let stats = self.handle.blocking.wake_round(&self.rand, &self.time);

            // kill_current_node() panics from blocking tasks are handed back by the
            // pool (their result-channel wrapper is on the killed node and cannot
            // deliver them); schedule the requested restarts. Each entry is an
            // independent per-node restart: a task's kill targets its own node, and two
            // tasks of the same node cannot both reach this in one round (turns run one
            // at a time, so the second is dropped by run_one's is_killed() check once
            // the first kill lands), so handling every entry is correct.
            for (node_id, err) in stats.panics {
                self.handle_task_panic(node_id, err);
            }

            // A wake round that started a blocking task, or that scheduled new
            // runnables (e.g. a completed blocking task waking its JoinHandle), is
            // progress in itself; time must not advance in that case, because a
            // blocking task waiting at a yield point may be unblocked by the new work.
            let progress = stats.dequeued > 0 || !self.queue.is_empty();
            if progress {
                stalled_rounds = 0;
            } else {
                let going = self.time.advance_to_next_event();
                if going {
                    stalled_rounds = 0;
                } else if stats.pending {
                    stalled_rounds += 1;
                    assert!(
                        stalled_rounds < MAX_STALLED_ROUNDS,
                        "deadlock: blocking task(s) stayed parked at a yield point for \
                         {} wake rounds with no other events to advance the simulation. \
                         A blocking wait on the pool (e.g. `blocking_recv`, or a lock \
                         acquire that spins with `yield_blocking`) is waiting for \
                         something that is never produced. Fix the program so the awaited \
                         value/lock is eventually made available, or so the wait can \
                         otherwise complete.",
                        MAX_STALLED_ROUNDS,
                    );
                } else {
                    panic!("no events, the task will block forever");
                }
            }
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

    /// Handle a panic payload that escaped a task of `node_id`: a `PanicWrapper` (from
    /// `kill_current_node`) schedules the requested restart; any other payload
    /// propagates and fails the test.
    fn handle_task_panic(&self, node_id: NodeId, err: Box<dyn std::any::Any + Send>) {
        if let Some(panic_info) = err.downcast_ref::<PanicWrapper>() {
            if let Some(restart_after) = panic_info.restart_after {
                let task = self.spawn_on_main_task(async move {
                    crate::time::sleep(restart_after).await;

                    let handle = runtime::Handle::current();
                    // the node may have been deleted by the test harness
                    // before the restart timer fires.
                    if handle.task.get_node(node_id).is_some() {
                        info!("restarting node {}", node_id);
                        runtime::Handle::current().restart(node_id);
                    }
                });

                task.fallible().detach();
            }
        } else {
            std::panic::resume_unwind(err);
        }
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

            if let Err(err) = result {
                self.handle_task_panic(node_id, err);
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

impl Drop for Executor {
    fn drop(&mut self) {
        self.handle.blocking.shutdown();

        // Runnables parked by `pause()` sit in `Node::paused` until the node map is
        // dropped. That map is shared with `runtime::Handle`, so it outlives the executor
        // and is torn down on the main thread after `block_on` has returned and the
        // context is gone. Dropping a runnable drops its future, whose Drop impls can make
        // intercepted syscalls (e.g. closing a socket or file); those would reach
        // `context::current()` with no reactor and panic inside an `extern "C"` fn, which
        // cannot unwind and aborts the process. Drain the map here with intercepts
        // disabled so the late syscalls reach the real libc. Same reasoning as
        // `PoolShared::drop`; the interceptors stay strict everywhere else.
        let _no_intercepts = crate::sim::intercept::disable_intercepts_scoped();
        self.handle.nodes.lock().unwrap().clear();
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
    blocking: Arc<BlockingPool>,
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

        // in-flight blocking tasks of this node unwind at their next yield point.
        self.blocking.request_abort_node(id);
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
                blocking: self.blocking.clone(),
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
            blocking: self.blocking.clone(),
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
            blocking: self.blocking.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct TaskNodeHandle {
    sender: mpsc::Sender<(Runnable, Arc<TaskInfo>)>,
    info: Arc<TaskInfo>,
    blocking: Arc<BlockingPool>,
}

assert_send_sync!(TaskNodeHandle);

impl TaskNodeHandle {
    pub fn current() -> Self {
        Self::try_current().unwrap()
    }

    pub fn try_current() -> Option<Self> {
        let info = crate::context::try_current_task()?;
        let (sender, blocking) =
            crate::context::try_current(|h| (h.task.sender.clone(), h.task.blocking.clone()))?;
        Some(TaskNodeHandle {
            sender,
            info,
            blocking,
        })
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

    /// Run a closure on the blocking pool, returning a JoinHandle for the result.
    ///
    /// The closure runs on a real OS thread, but only when granted a turn by the
    /// executor (see [`blocking`]). The returned handle is backed by an ordinary async
    /// task on this node, so abort and kill-on-node-death behave as for other tasks.
    pub fn spawn_blocking<F, R>(&self, f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: blocking::BlockingFn = Box::new(move || {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                Ok(v) => {
                    let _ = tx.send(Ok(v));
                }
                Err(e) => {
                    if e.is::<blocking::AbortBlockingTask>() || e.is::<PanicWrapper>() {
                        // Propagate to the pool thread loop. An abort unwind is simply
                        // ignored there. A PanicWrapper (kill_current_node) must be
                        // handed to the main loop via the pool: it kills this task's
                        // own node, so the wrapper task below is dead and cannot
                        // deliver the payload (and the restart would be lost).
                        std::panic::resume_unwind(e);
                    }
                    // other panics are re-thrown on the main thread by the wrapper
                    // task below, where run_all_ready's existing handling applies.
                    let _ = tx.send(Err(e));
                }
            }
        });
        self.blocking.spawn(job, self.info.clone());
        self.spawn(async move {
            match rx.await {
                Ok(Ok(v)) => v,
                Ok(Err(payload)) => std::panic::resume_unwind(payload),
                // the sender is dropped without sending only when the node is killed,
                // in which case this task is killed as well and never polled again.
                Err(_) => unreachable!("blocking task aborted but its node is alive"),
            }
        })
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
    handle.spawn_blocking(f)
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
    id: Id,
    inner: Arc<InnerHandle<T>>,
}

impl<T> JoinHandle<T> {
    /// Returns a task ID that uniquely identifies this task relative to other currently spawned
    /// tasks.
    pub fn id(&self) -> Id {
        self.id
    }

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
    id: Id,
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

    #[test]
    fn spawn_blocking_basic() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let v = spawn_blocking(|| 40 + 2).await.unwrap();
            assert_eq!(v, 42);
        });
    }

    #[test]
    fn spawn_blocking_yield_wait() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let flag = Arc::new(AtomicBool::new(false));

            let flag1 = flag.clone();
            let blocking = spawn_blocking(move || {
                while !flag1.load(Ordering::SeqCst) {
                    yield_blocking();
                }
                123
            });

            // before the blocking pool existed, the blocking task would run inline and
            // spin forever, as this timer could never fire.
            let flag2 = flag.clone();
            spawn(async move {
                time::sleep(Duration::from_secs(1)).await;
                flag2.store(true, Ordering::SeqCst);
            });

            assert_eq!(blocking.await.unwrap(), 123);
        });
    }

    #[test]
    fn spawn_blocking_cross_task_wait() {
        // two blocking tasks that wait on each other's progress via yield points.
        let runtime = Runtime::new();
        runtime.block_on(async {
            let flag = Arc::new(AtomicBool::new(false));

            let flag1 = flag.clone();
            let waiter = spawn_blocking(move || {
                while !flag1.load(Ordering::SeqCst) {
                    yield_blocking();
                }
            });

            let flag2 = flag.clone();
            let setter = spawn_blocking(move || {
                // yield a few times before setting the flag, so the waiter parks.
                for _ in 0..3 {
                    yield_blocking();
                }
                flag2.store(true, Ordering::SeqCst);
            });

            setter.await.unwrap();
            waiter.await.unwrap();
        });
    }

    #[test]
    fn spawn_blocking_nested() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let v = spawn_blocking(|| {
                let inner = spawn_blocking(|| 7);
                // wait for the inner task from the pool thread via yield points.
                while !inner.is_finished() {
                    yield_blocking();
                }
                8
            })
            .await
            .unwrap();
            assert_eq!(v, 8);
        });
    }

    #[test]
    fn spawn_blocking_deterministic_order() {
        let run = |seed: u64| {
            let runtime = Runtime::with_seed(seed);
            runtime.block_on(async {
                let order = Arc::new(Mutex::new(Vec::new()));
                let mut handles = Vec::new();
                for i in 0..10 {
                    let order = order.clone();
                    handles.push(spawn_blocking(move || {
                        order.lock().unwrap().push(i);
                        yield_blocking();
                        order.lock().unwrap().push(i + 100);
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
                let order = order.lock().unwrap().clone();
                order
            })
        };
        let a = run(42);
        let b = run(42);
        assert_eq!(a, b);
        assert_eq!(a.len(), 20);
    }

    #[test]
    #[should_panic(expected = "boom in blocking task")]
    fn spawn_blocking_panic_propagates() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let _ = spawn_blocking(|| panic!("boom in blocking task")).await;
        });
    }

    struct SetOnDrop(Arc<AtomicBool>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn spawn_blocking_kill_unwinds_parked_task() {
        let runtime = Runtime::new();
        let node = runtime.create_node().build();
        let dropped = Arc::new(AtomicBool::new(false));

        let dropped1 = dropped.clone();
        let handle = node.spawn_blocking(move || {
            let _guard = SetOnDrop(dropped1);
            loop {
                yield_blocking();
            }
        });

        runtime.block_on(async move {
            time::sleep(Duration::from_secs(1)).await;
            assert!(!dropped.load(Ordering::SeqCst));
            Handle::current().kill(node.id());
            // the abort is delivered at the next wake round; the task unwinds,
            // running its Drop impls on the pool thread.
            time::sleep(Duration::from_secs(1)).await;
            assert!(dropped.load(Ordering::SeqCst));
            assert!(handle.await.is_err());
        });
    }

    #[test]
    fn spawn_blocking_shutdown_unwinds_parked_task() {
        let runtime = Runtime::new();
        let dropped = Arc::new(AtomicBool::new(false));

        let dropped1 = dropped.clone();
        runtime.block_on(async {
            spawn_blocking(move || {
                let _guard = SetOnDrop(dropped1);
                loop {
                    yield_blocking();
                }
            });
            // return with the blocking task still parked.
            time::sleep(Duration::from_secs(1)).await;
        });

        assert!(!dropped.load(Ordering::SeqCst));
        drop(runtime);
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_blocking_kill_current_node_restarts() {
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

            // kill from a blocking task: the PanicWrapper must reach the main loop
            // via the pool (the task's own wrapper is killed with the node), so that
            // the restart is scheduled.
            node.spawn_blocking(|| {
                kill_current_node(Some(Duration::from_secs(2)));
            });

            // killed at ~3s; restart timer fires at ~5s, resetting the flag.
            time::sleep_until(t0 + Duration::from_secs(6)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 0);

            time::sleep_until(t0 + Duration::from_secs(8)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    #[should_panic(expected = "deadlock")]
    fn spawn_blocking_deadlock_detected() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            // parked forever, and no timers exist that could unblock it.
            spawn_blocking(|| loop {
                yield_blocking();
            })
            .await
            .unwrap();
        });
    }
}
