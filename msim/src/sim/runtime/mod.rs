//! The msim runtime.

use super::*;
use crate::assert_send_sync;
use crate::context::TaskEnterGuard;
use crate::net::NetSim;
use crate::task::{JoinHandle, NodeId};
use ::rand::Rng;
use std::{
    any::TypeId,
    collections::HashMap,
    fmt,
    future::Future,
    io::Write,
    net::IpAddr,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::sync::oneshot;

use tracing::{debug, error, trace, warn};

pub(crate) mod context;

/// The msim runtime.
///
/// The runtime provides basic components for deterministic simulation,
/// including a [random number generator], [timer], [task scheduler], and
/// simulated [network] and [file system].
///
/// [random number generator]: crate::rand
/// [timer]: crate::time
/// [task scheduler]: crate::task
/// [network]: crate::net
/// [file system]: crate::fs
pub struct Runtime {
    rand: rand::GlobalRng,
    task: task::Executor,
    handle: Handle,
}

assert_send_sync!(Runtime);

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a new runtime instance with default seed and config.
    pub fn new() -> Self {
        Self::with_seed(0)
    }

    /// Create a new runtime instance with a given seed and default config.
    pub fn with_seed(seed: u64) -> Self {
        Self::with_seed_and_config(seed, SimConfig::default())
    }

    /// Create a new runtime instance with given seed and config.
    pub fn with_seed_and_config(seed: u64, config: SimConfig) -> Self {
        let mut rand = rand::GlobalRng::new_with_seed(seed);
        tokio::msim_adapter::util::reset_rng(rand.gen::<u64>());
        let task = task::Executor::new(rand.clone());
        let handle = Handle {
            rand: rand.clone(),
            time: task.time_handle().clone(),
            task: task.handle().clone(),
            sims: Default::default(),
            config,
        };
        let rt = Runtime { rand, task, handle };
        rt.add_simulator::<fs::FsSim>();
        rt.add_simulator::<net::NetSim>();
        intercept::enable_intercepts(true);
        rt
    }

    /// Register a simulator.
    pub fn add_simulator<S: plugin::Simulator>(&self) {
        let mut sims = self.handle.sims.lock().unwrap();
        let sim = Arc::new(S::new(
            &self.handle.rand,
            &self.handle.time,
            &self.handle.config,
        ));
        // create node for supervisor
        sim.create_node(NodeId::zero());
        sims.insert(TypeId::of::<S>(), sim);
    }

    /// Return a handle to the runtime.
    ///
    /// The returned handle can be used by the supervisor (future in [block_on])
    /// to control the whole system. For example, kill a node or disconnect the
    /// network.
    ///
    /// [block_on]: Runtime::block_on
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// Create a node.
    ///
    /// The returned handle can be used to spawn tasks that run on this node.
    pub fn create_node(&self) -> NodeBuilder<'_> {
        self.handle.create_node()
    }

    /// Run a future to completion on the runtime. This is the runtime’s entry point.
    ///
    /// This runs the given future on the current thread until it is complete.
    ///
    /// # Example
    ///
    /// ```
    /// use msim::runtime::Runtime;
    ///
    /// let rt = Runtime::new();
    /// let ret = rt.block_on(async { 1 });
    /// assert_eq!(ret, 1);
    /// ```
    ///
    /// Unlike usual async runtime, when there is no runnable task, it will
    /// panic instead of blocking.
    ///
    /// ```should_panic
    /// use msim::runtime::Runtime;
    /// use futures::future::pending;
    ///
    /// Runtime::new().block_on(pending::<()>());
    /// ```
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let _guard = crate::context::enter(self.handle.clone());
        crate::time::ensure_clocks();
        self.task.block_on(future)
    }

    /// Set a time limit of the execution.
    ///
    /// The runtime will panic when time limit exceeded.
    ///
    /// # Example
    ///
    /// ```should_panic
    /// use msim::{runtime::Runtime, time::{sleep, Duration}};
    ///
    /// let mut rt = Runtime::new();
    /// rt.set_time_limit(Duration::from_secs(1));
    ///
    /// rt.block_on(async {
    ///     sleep(Duration::from_secs(2)).await;
    /// });
    /// ```
    pub fn set_time_limit(&mut self, limit: Duration) {
        self.task.set_time_limit(limit);
    }

    /// Enable determinism check during the simulation.
    ///
    /// # Example
    ///
    /// ```should_panic
    /// use msim::{runtime::Runtime, time::{sleep, Duration}};
    /// use rand::Rng;
    ///
    /// let f = || async {
    ///     for _ in 0..10 {
    ///         msim::rand::thread_rng().gen::<u64>();
    ///         // introduce non-determinism
    ///         let rand_num = rand::thread_rng().gen_range(0..10);
    ///         sleep(Duration::from_nanos(rand_num)).await;
    ///     }
    /// };
    ///
    /// let mut rt = Runtime::new();
    /// rt.enable_determinism_check(None);      // enable log
    /// rt.block_on(f());
    /// let log = rt.take_rand_log();           // take log for next turn
    ///
    /// let mut rt = Runtime::new();
    /// rt.enable_determinism_check(log);       // enable check
    /// rt.block_on(f());                       // run the same logic again,
    ///                                         // should panic here.
    /// ```
    pub fn enable_determinism_check(&self, log: Option<rand::Log>) {
        assert_eq!(
            self.task.time_handle().time_since_clock_base(),
            Duration::from_secs(0),
            "deterministic check must be set at init"
        );
        if let Some(log) = log {
            self.rand.enable_check(log);
        } else {
            self.rand.enable_log();
        }
    }

    /// Take random log so that you can check determinism in the next turn.
    pub fn take_rand_log(self) -> Option<rand::Log> {
        self.rand.take_log()
    }
}

/// Start a watch dog thread that will kill the test process in case of a deadlock.
pub fn start_watchdog(
    rt: Arc<RwLock<Option<Runtime>>>,
    inner_seed: u64,
    timeout: Duration,
    stop: oneshot::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    start_watchdog_with(rt, timeout, stop, move || {
        error!("deadlock detected, aborting()");
        println!(
            "note: run with `MSIM_TEST_SEED={}` environment variable to reproduce this error",
            inner_seed
        );
        let _ = std::io::stdout().flush();
        std::process::abort();
    })
}

fn start_watchdog_with(
    rt: Arc<RwLock<Option<Runtime>>>,
    timeout: Duration,
    mut stop: oneshot::Receiver<()>,
    on_deadlock: impl FnOnce() + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let limit = 10;
    let step = timeout / limit;

    std::thread::spawn(move || {
        if std::env::var("MSIM_DISABLE_WATCHDOG").is_ok() {
            warn!("simulator watchdog thread disabled due to MSIM_DISABLE_WATCHDOG");
            stop.blocking_recv().expect("watchdog stop tx was dropped");
            return;
        }

        debug!(tid = ?std::thread::current().id(),
            "watchdog thread starting. to disable set MSIM_DISABLE_WATCHDOG=1");
        let read = rt.read().unwrap();
        let rt = &(*read).as_ref().unwrap();
        let mut prev_time = rt.handle.time.now_instant();
        let mut deadlock_count = 0;
        loop {
            std::thread::sleep(step);
            if stop.try_recv().is_ok() {
                break;
            }

            let now = rt.handle.time.now_instant();
            if now < prev_time {
                eprintln!("clock went backwards! {:?} {:?}", now, prev_time);
                let _ = std::io::stdout().flush();
                std::process::abort();
            }
            if now == prev_time {
                warn!("possible deadlock detected...");
                deadlock_count += 1;
            } else if deadlock_count > 0 {
                warn!("deadlock cleared (perhaps the process was paused)...");
                deadlock_count = 0;
            }
            prev_time = now;

            // we wait until we've seen the clock not advance 10 times in a
            // row, so that we don't get spurious panics when the process is
            // paused in a debugger.
            if deadlock_count > limit {
                on_deadlock();
                return;
            }
        }
        debug!(tid = ?std::thread::current().id(), "watchdog thread exiting");
    })
}

/// Supervisor handle to the runtime.
#[derive(Clone)]
pub struct Handle {
    pub(crate) rand: rand::GlobalRng,
    pub(crate) time: time::TimeHandle,
    pub(crate) task: task::TaskHandle,
    pub(crate) sims: Arc<Mutex<HashMap<TypeId, Arc<dyn plugin::Simulator>>>>,
    pub(crate) config: SimConfig,
}

impl Handle {
    /// Returns a [`Handle`] view over the currently running [`Runtime`].
    ///
    /// ## Panic
    ///
    /// This will panic if called outside the context of a Madsim runtime.
    ///
    /// ```should_panic
    /// let handle = msim::runtime::Handle::current();
    /// ```
    pub fn current() -> Self {
        context::current(|h| h.clone())
    }

    /// Returns a handle if there is any active
    pub fn try_current() -> Option<Self> {
        context::try_current(|h| h.clone())
    }

    /// Kill a node.
    ///
    /// - All tasks spawned on this node will be killed immediately.
    /// - All data that has not been flushed to the disk will be lost.
    pub fn kill(&self, id: NodeId) {
        self.task.kill(id);
        for sim in self.sims.lock().unwrap().values() {
            sim.reset_node(id);
        }
    }

    /// Restart a node。
    pub fn restart(&self, id: NodeId) {
        self.task.restart(id);
        for sim in self.sims.lock().unwrap().values() {
            sim.reset_node(id);
        }
    }

    /// Kill all tasks and delete the node.
    pub fn delete_node(&self, id: NodeId) {
        debug!("delete_node {id}");
        self.task.delete_node(id);
        for sim in self.sims.lock().unwrap().values() {
            sim.delete_node(id);
        }
    }

    /// Pause the execution of a node.
    pub fn pause(&self, id: NodeId) {
        self.task.pause(id);
    }

    /// Resume the execution of a node.
    pub fn resume(&self, id: NodeId) {
        self.task.resume(id);
    }

    /// Create a node which will be bound to the specified address.
    pub fn create_node(&self) -> NodeBuilder<'_> {
        NodeBuilder::new(self)
    }

    /// Return a handle of the specified node.
    pub fn get_node(&self, id: NodeId) -> Option<NodeHandle> {
        self.task.get_node(id).map(|task| NodeHandle { task })
    }

    /// Mark this Handle as the currently active one
    pub fn enter(self) -> EnterGuard {
        EnterGuard(context::enter(self))
    }

    /// Get the TimeHandle
    pub fn time(&self) -> &time::TimeHandle {
        &self.time
    }
}

/// Guard for entering handle
pub struct EnterGuard(context::EnterGuard);

/// Builds a node with custom configurations.
pub struct NodeBuilder<'a> {
    handle: &'a Handle,
    name: Option<String>,
    ip: Option<IpAddr>,
    init: Option<Arc<dyn Fn(&task::TaskNodeHandle) + Send + Sync>>,
}

impl<'a> NodeBuilder<'a> {
    fn new(handle: &'a Handle) -> Self {
        NodeBuilder {
            handle,
            name: None,
            ip: None,
            init: None,
        }
    }

    /// Names the node.
    ///
    /// The default name is node ID.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the initial task for the node.
    ///
    /// This task will be automatically respawned after crash.
    pub fn init<F>(mut self, future: impl Fn() -> F + Send + Sync + 'static) -> Self
    where
        F: Future + 'static,
    {
        self.init = Some(Arc::new(move |handle| {
            handle.spawn_local(future());
        }));
        self
    }

    /// Set one IP address of the node.
    pub fn ip(mut self, ip: IpAddr) -> Self {
        self.ip = Some(ip);
        self
    }

    /// Build a node.
    pub fn build(self) -> NodeHandle {
        let task = self.handle.task.create_node(self.name, self.init);
        for sim in self.handle.sims.lock().unwrap().values() {
            sim.create_node(task.id());
            if let Some(ip) = self.ip {
                if let Some(net) = sim.downcast_ref::<net::NetSim>() {
                    net.set_ip(task.id(), ip)
                }
            }
        }
        NodeHandle { task }
    }
}

/// Guard for entering a node context.
#[must_use]
pub struct NodeEnterGuard(TaskEnterGuard);

/// Handle to a node.
#[derive(Clone)]
pub struct NodeHandle {
    task: task::TaskNodeHandle,
}

impl NodeHandle {
    /// Get handle to current Node
    pub fn current() -> Self {
        Self::try_current().unwrap()
    }

    /// Get handle to current Node if there is one
    pub fn try_current() -> Option<Self> {
        let task = task::TaskNodeHandle::try_current()?;
        Some(Self { task })
    }

    /// Returns the node ID.
    pub fn id(&self) -> NodeId {
        self.task.id()
    }

    /// Spawn a future onto the runtime.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.task.spawn(future)
    }

    /// Spawn a blocking task.
    pub fn spawn_blocking<F, R>(&self, f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.task.spawn(async move { f() })
    }

    /// Spawn a on the local thread.
    pub fn spawn_local<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        self.task.spawn_local(future)
    }

    /// Join the node.
    /// TODO: unimplemented
    pub fn join(self) -> Result<(), ()> {
        warn!("TODO: implement NodeHandle::join()");
        Ok(())
    }

    /// Get ip of node
    pub fn ip(&self) -> Option<IpAddr> {
        let net = plugin::simulator::<NetSim>();
        net.get_ip(self.id())
    }

    /// await a future in a node. This is equivalent to calling
    /// NodeHandle::spawn(fut).await.unwrap(), except without the requirement that everything
    /// involved is Send + 'static.
    pub async fn await_future_in_node<F: Future>(&self, fut: F) -> F::Output {
        self.task.await_future_in_node(fut).await
    }

    /// Enter the node - all tasks spawned, network connections created, etc while the guard is
    /// held will come from the node in question.
    pub fn enter_node(&self) -> NodeEnterGuard {
        NodeEnterGuard(self.task.enter())
    }
}

impl fmt::Debug for NodeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeHandle({})", self.id())
    }
}

impl Future for NodeHandle {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // Provide a way for nodes to exit
        std::task::Poll::Pending
    }
}

/// Checks if current task is killed (or non-existent) - only intended to be used by
/// return_if_killed!
pub fn is_current_task_killed() -> bool {
    if let Some(task) = context::try_current_task() {
        if task.is_killed() {
            trace!("current task is killed");
            true
        } else {
            false
        }
    } else {
        trace!("no current task");
        true
    }
}

/// Return from the current function if the current task is killed, or there is no current task.
/// Intended mainly for Drop impls, so assumes that the current function returns ().
#[macro_export]
macro_rules! return_if_killed {
    () => {
        if $crate::runtime::is_current_task_killed() {
            tracing::trace!("early return - task is killed");
            return;
        }
    };
}

/// Initialize logger.
pub fn init_logger() {
    use std::sync::Once;
    static LOGGER_INIT: Once = Once::new();
    LOGGER_INIT.call_once(tracing_subscriber::fmt::init);
}

#[cfg(test)]
mod tests {
    use super::start_watchdog_with;
    use crate::{runtime::Runtime, time};
    use std::{
        sync::{Arc, RwLock},
        time::Duration,
    };
    use tokio::sync::oneshot::channel;
    use tracing::{error, info};

    #[test]
    fn test_watchdog() {
        // This test will panic if logging is enabled since the logging happens outside of a
        // runtime. To debug this test, uncomment init_logger(), and run the test with
        // MSIM_USE_REAL_WALLCLOCK=1 in the env.
        //super::init_logger();

        let runtime = Arc::new(RwLock::new(Some(Runtime::new())));
        let (_tx, rx) = channel();

        let (deadlock_tx, deadlock_rx) = channel();
        let _watchdog =
            start_watchdog_with(runtime.clone(), Duration::from_secs(1), rx, move || {
                error!("deadlock detected");
                deadlock_tx.send(()).expect("cancel_rx dropped");
            });

        let now = std::time::Instant::now();
        std::thread::spawn(move || {
            let runtime = runtime.read().unwrap();

            runtime.as_ref().unwrap().block_on(async {
                std::thread::sleep(Duration::from_millis(800));
                // briefly come back to life to exercise the timer reset.
                time::sleep(Duration::from_secs(1)).await;

                std::thread::sleep(Duration::from_millis(5000));
                panic!("thread sleep finished without watchdog noticing");
            });
        });

        deadlock_rx.blocking_recv().expect("cancel_tx dropped");
        info!("deadlock detected successfully");
        // verify that the deadline was reset after we came back after the timer reset
        assert!(now.elapsed() > Duration::from_millis(1500));
    }
}
