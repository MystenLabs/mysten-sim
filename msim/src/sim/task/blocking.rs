// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Deterministic blocking task pool.
//!
//! `spawn_blocking` closures run on real OS threads, but a [`ThreadQuantum`] enforces
//! that at most one thread in the simulator (the main executor thread or one pool
//! thread) is running at any moment. Between polling rounds the main executor grants
//! each pool thread a turn, in seeded-random order, so execution remains deterministic.
//!
//! Blocking code that needs to wait for sim progress must wait via
//! [`yield_blocking`]-based primitives: an OS-level block on a pool thread never ends
//! its turn, which hangs the simulator.

use super::{NodeId, PanicWrapper, TaskInfo};
use crate::rand::GlobalRng;
use crate::runtime::Handle;
use crate::sim::utils::mpsc;
use crate::time::TimeRuntime;
use rand::seq::SliceRandom;
use rand::Rng;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tracing::trace;

pub(crate) type BlockingFn = Box<dyn FnOnce() + Send + 'static>;
type BlockingJob = (BlockingFn, Arc<TaskInfo>);

/// Panic payload used to unwind a blocking task whose node has been killed (or whose
/// pool is shutting down). Thrown from a yield point, caught by the pool thread loop.
pub(crate) struct AbortBlockingTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Idle,
    Run(u32),
    Abort(u32),
    Shutdown,
}

enum TurnKind {
    Run,
    Abort,
    Exit,
}

struct QuantumState {
    turn: Turn,
    /// The task each pool thread is currently executing (parked at a yield point), if any.
    current: Vec<Option<Arc<TaskInfo>>>,
    /// Deferred abort flags set by kill(); delivered as `Turn::Abort` at the next wake round.
    /// kill() may itself run on a pool thread, so it cannot perform a wake handshake.
    abort_requested: Vec<bool>,
    /// Tasks dequeued during the current wake round, for progress detection.
    dequeued: usize,
    /// PanicWrapper payloads (kill_current_node) that escaped blocking tasks, handed to
    /// the main loop each wake round so it can schedule the requested restarts.
    pending_panics: Vec<(NodeId, Box<dyn std::any::Any + Send>)>,
}

struct ThreadQuantum {
    state: Mutex<QuantumState>,
    cv: Condvar,
}

impl ThreadQuantum {
    fn new(num_threads: usize) -> Self {
        Self {
            state: Mutex::new(QuantumState {
                turn: Turn::Idle,
                current: vec![None; num_threads],
                abort_requested: vec![false; num_threads],
                dequeued: 0,
                pending_panics: Vec::new(),
            }),
            cv: Condvar::new(),
        }
    }

    /// Park until this thread is granted a turn. Never panics, so it is safe to call
    /// at the top of the pool thread loop (outside any task).
    fn wait_turn(&self, me: u32) -> TurnKind {
        let mut s = self.state.lock().unwrap();
        loop {
            match s.turn {
                Turn::Shutdown => return TurnKind::Exit,
                Turn::Run(i) if i == me => return TurnKind::Run,
                Turn::Abort(i) if i == me => return TurnKind::Abort,
                _ => s = self.cv.wait(s).unwrap(),
            }
        }
    }

    /// End this thread's turn. Preserves `Shutdown` so other threads still observe it.
    fn end_turn(&self, _me: u32) {
        let mut s = self.state.lock().unwrap();
        if s.turn != Turn::Shutdown {
            s.turn = Turn::Idle;
            self.cv.notify_all();
        }
    }

    /// Yield mid-task: give up the current turn and park until the next one.
    /// Unwinds the task (via panic) if the node was killed or the pool is shutting down.
    fn yield_turn(&self, me: u32) {
        self.end_turn(me);
        match self.wait_turn(me) {
            TurnKind::Run => {}
            TurnKind::Abort | TurnKind::Exit => {
                self.state.lock().unwrap().current[me as usize] = None;
                std::panic::panic_any(AbortBlockingTask);
            }
        }
    }

    fn note_dequeue(&self, me: u32, info: Option<Arc<TaskInfo>>) {
        let mut s = self.state.lock().unwrap();
        s.dequeued += 1;
        s.current[me as usize] = info;
    }

    fn note_complete(&self, me: u32) {
        let mut s = self.state.lock().unwrap();
        s.current[me as usize] = None;
        // the abort target no longer exists.
        s.abort_requested[me as usize] = false;
    }
}

/// Result of a wake round, used by `block_on` for progress/deadlock detection.
pub(crate) struct RoundStats {
    /// Number of tasks dequeued (started) during the round.
    pub dequeued: usize,
    /// Whether any blocking work remains (parked mid-task threads or queued tasks).
    pub pending: bool,
    /// PanicWrapper payloads that escaped blocking tasks this round, for the main loop
    /// to schedule node restarts.
    pub panics: Vec<(NodeId, Box<dyn std::any::Any + Send>)>,
}

struct PoolShared {
    queue: mpsc::Receiver<BlockingJob>,
    sender: mpsc::Sender<BlockingJob>,
    quantum: ThreadQuantum,
    rand: GlobalRng,
    num_threads: u32,
}

pub(crate) struct BlockingPool {
    shared: Arc<PoolShared>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    started: AtomicBool,
    /// Runtime handle entered by pool threads at startup; see [`Self::set_handle`].
    handle: Mutex<Option<Handle>>,
}

thread_local! {
    static POOL_THREAD: RefCell<Option<(u32, Arc<PoolShared>)>> = const { RefCell::new(None) };
}

/// Yield the current blocking-pool thread's execution quantum, allowing other threads
/// (and the main executor) to run. Blocking code that waits for progress made elsewhere
/// in the simulation must call this in its wait loop instead of blocking the OS thread.
///
/// Unwinds the current task (via panic) if its node has been killed.
///
/// Panics if called from outside a blocking-pool thread. No-op while panicking, so that
/// Drop impls running during an unwind cannot double-panic.
pub fn yield_blocking() {
    if std::thread::panicking() {
        return;
    }
    POOL_THREAD.with(|p| {
        let p = p.borrow();
        let (me, shared) = p
            .as_ref()
            .expect("yield_blocking() called from outside a blocking-pool thread");
        shared.quantum.yield_turn(*me);
    });
}

impl BlockingPool {
    pub fn new(rand: GlobalRng) -> Self {
        // The pool must be large enough that tasks parked at yield points (waiting for
        // sim progress) cannot occupy every thread and starve the task that would
        // unblock them. Threads are cheap: they are spawned lazily, parked when idle,
        // and idle threads are skipped by wake rounds.
        let num_threads = std::env::var("MSIM_BLOCKING_THREADS")
            .ok()
            .map(|v| {
                v.parse::<u32>()
                    .expect("MSIM_BLOCKING_THREADS must be a positive integer")
            })
            .unwrap_or(128);
        assert!(num_threads > 0, "MSIM_BLOCKING_THREADS must be >= 1");

        let (sender, queue) = mpsc::channel();
        Self {
            shared: Arc::new(PoolShared {
                queue,
                sender,
                quantum: ThreadQuantum::new(num_threads as usize),
                rand,
                num_threads,
            }),
            threads: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
            handle: Mutex::new(None),
        }
    }

    /// Store the runtime handle that pool threads enter on startup. Called once during
    /// runtime construction. (The handle contains this pool via TaskHandle, creating an
    /// Arc cycle; `shutdown` clears it so the cycle only lives until runtime drop.)
    pub fn set_handle(&self, handle: Handle) {
        *self.handle.lock().unwrap() = Some(handle);
    }

    /// Enqueue a blocking job, lazily spawning the pool threads on first use.
    pub fn spawn(&self, f: BlockingFn, info: Arc<TaskInfo>) {
        self.ensure_started();
        self.shared
            .sender
            .send((f, info))
            .unwrap_or_else(|_| panic!("blocking pool queue is closed"));
    }

    fn ensure_started(&self) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        let handle = self
            .handle
            .lock()
            .unwrap()
            .clone()
            .expect("blocking pool used before runtime construction completed");
        let mut threads = self.threads.lock().unwrap();
        for i in 0..self.shared.num_threads {
            let shared = self.shared.clone();
            let handle = handle.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("msim-blocking-{}", i))
                    .spawn(move || run_blocking_thread(i, shared, handle))
                    .expect("failed to spawn blocking pool thread"),
            );
        }
    }

    /// Give each pool thread one turn, in seeded-random order. Called by the main
    /// executor loop between polling rounds; returns progress stats.
    pub fn wake_round(&self, rand: &GlobalRng, time: &TimeRuntime) -> RoundStats {
        if !self.started.load(Ordering::Relaxed) {
            return RoundStats {
                dequeued: 0,
                pending: false,
                panics: Vec::new(),
            };
        }

        // fast path: nothing to run, resume or abort. (Skipping the shuffle draw is
        // deterministic, since the pool state itself is deterministic.)
        {
            let mut s = self.shared.quantum.state.lock().unwrap();
            if s.current.iter().all(|c| c.is_none())
                && !s.abort_requested.iter().any(|&a| a)
                && self.shared.queue.is_empty()
            {
                return RoundStats {
                    dequeued: 0,
                    pending: false,
                    panics: std::mem::take(&mut s.pending_panics),
                };
            }
        }

        let mut order: Vec<u32> = (0..self.shared.num_threads).collect();
        rand.with(|rng| order.shuffle(rng));

        // Suppress panic-hook output for the controlled unwinds that can occur on pool
        // threads during this round. Installed lazily, only if a turn is granted.
        let mut hook_guard: Option<Arc<super::PanicHookGuard>> = None;

        self.shared.quantum.state.lock().unwrap().dequeued = 0;

        for i in order {
            let mut s = self.shared.quantum.state.lock().unwrap();
            let turn = if s.abort_requested[i as usize] {
                s.abort_requested[i as usize] = false;
                Turn::Abort(i)
            } else if let Some(info) = &s.current[i as usize] {
                if info.paused.load(Ordering::SeqCst) {
                    continue;
                }
                Turn::Run(i)
            } else if self.shared.queue.is_empty() {
                // idle thread with nothing to pick up
                continue;
            } else {
                Turn::Run(i)
            };

            if hook_guard.is_none() {
                hook_guard = Some(install_pool_panic_hook());
            }

            trace!("granting turn {:?} to blocking thread {}", turn, i);
            s.turn = turn;
            self.shared.quantum.cv.notify_all();
            let _s = self
                .shared
                .quantum
                .cv
                .wait_while(s, |s| s.turn != Turn::Idle)
                .unwrap();
            drop(_s);

            let dur = Duration::from_nanos(rand.with(|rng| rng.gen_range(50..100)));
            time.advance(dur);
        }

        let mut s = self.shared.quantum.state.lock().unwrap();
        RoundStats {
            dequeued: s.dequeued,
            pending: s.current.iter().any(|c| c.is_some()) || !self.shared.queue.is_empty(),
            panics: std::mem::take(&mut s.pending_panics),
        }
    }

    /// Request that any thread currently executing a task of `node` unwinds it at the
    /// start of its next turn. Callable from any thread (including pool threads).
    pub fn request_abort_node(&self, node: super::NodeId) {
        let mut s = self.shared.quantum.state.lock().unwrap();
        for i in 0..s.current.len() {
            if s.current[i]
                .as_ref()
                .is_some_and(|info| info.node() == node)
            {
                s.abort_requested[i] = true;
            }
        }
    }

    /// Shut down the pool: unwind parked tasks and join all threads.
    pub fn shutdown(&self) {
        // break the Arc cycle described in set_handle.
        self.handle.lock().unwrap().take();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap());
        if threads.is_empty() {
            return;
        }
        // Suppress panic-hook output for the AbortBlockingTask unwinds of parked tasks.
        // The hook cannot be touched if we are already unwinding (e.g. the runtime is
        // dropped by a failing test) - std forbids modifying it from a panicking
        // thread; the parked tasks then unwind with default-hook noise, which is fine.
        let _hook_guard = if std::thread::panicking() {
            None
        } else {
            Some(install_pool_panic_hook())
        };
        {
            let mut s = self.shared.quantum.state.lock().unwrap();
            s.turn = Turn::Shutdown;
            self.shared.quantum.cv.notify_all();
        }
        for t in threads {
            t.join().expect("blocking pool thread panicked");
        }
    }
}

fn install_pool_panic_hook() -> Arc<super::PanicHookGuard> {
    let hook_guard = Arc::new(super::PanicHookGuard::new());
    let hook_guard_clone = Arc::downgrade(&hook_guard);
    std::panic::set_hook(Box::new(move |panic_info| {
        let payload = panic_info.payload();
        if payload.downcast_ref::<super::PanicWrapper>().is_none()
            && payload.downcast_ref::<AbortBlockingTask>().is_none()
        {
            if let Some(old_hook) = hook_guard_clone.upgrade() {
                old_hook.call_hook(panic_info);
            }
        }
    }));
    hook_guard
}

fn run_blocking_thread(me: u32, shared: Arc<PoolShared>, handle: Handle) {
    // Install the sim environment for the lifetime of the thread, so that blocking code
    // (and Drop impls run during unwinds) can use sim time, rand and task context.
    let _ctx = crate::context::enter(handle);
    crate::sim::intercept::enable_intercepts_quiet(true);
    // TLS destructors run after the context guard is dropped; an intercepted call there
    // (with intercepts enabled but no context) panics inside an extern "C" fn, which
    // cannot unwind and aborts the process. Route such calls back to the real libc.
    struct DisableInterceptsOnExit;
    impl Drop for DisableInterceptsOnExit {
        fn drop(&mut self) {
            crate::sim::intercept::enable_intercepts_quiet(false);
        }
    }
    let _disable_intercepts = DisableInterceptsOnExit;
    crate::time::ensure_clocks();
    POOL_THREAD.with(|p| *p.borrow_mut() = Some((me, shared.clone())));

    loop {
        match shared.quantum.wait_turn(me) {
            TurnKind::Exit => break,
            TurnKind::Run => {}
            // aborts are only delivered to parked mid-task threads, which wait inside
            // yield_turn; nothing to unwind here.
            TurnKind::Abort => {
                shared.quantum.end_turn(me);
                continue;
            }
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_one(&shared, me);
        }));
        if let Err(err) = result {
            if err.is::<AbortBlockingTask>() {
                // controlled unwind of a killed task - nothing to do.
            } else if err.is::<PanicWrapper>() {
                // kill_current_node() was called by this task. Its own node is being
                // killed, so its result-channel wrapper cannot deliver the payload;
                // hand it to the main loop, which schedules the requested restart.
                let mut s = shared.quantum.state.lock().unwrap();
                let info = s.current[me as usize]
                    .take()
                    .expect("a panicking task must have been registered as current");
                s.abort_requested[me as usize] = false;
                s.pending_panics.push((info.node(), err));
            } else {
                // Jobs catch their own panics and forward them through their result
                // channel; nothing else may reach this point.
                eprintln!("unexpected panic escaped a blocking task; aborting");
                std::process::abort();
            }
        }

        shared.quantum.end_turn(me);
    }
}

/// Dequeue and run a single blocking job. One job per turn keeps interleaving with
/// async tasks fine-grained.
fn run_one(shared: &Arc<PoolShared>, me: u32) {
    let (f, info) = match shared.queue.try_recv_random(&shared.rand) {
        Ok(job) => job,
        Err(_) => return,
    };

    if info.is_killed() {
        // must enter the task before dropping the job, so that Drop impls can run.
        shared.quantum.note_dequeue(me, None);
        let _guard = crate::context::enter_task(info);
        drop(f);
        return;
    }

    if info.paused.load(Ordering::SeqCst) {
        // requeue without counting progress: a paused task must not prevent the
        // executor from advancing time (it is unpaused by a future event).
        let _ = shared.sender.send((f, info));
        return;
    }

    shared.quantum.note_dequeue(me, Some(info.clone()));
    let _guard = crate::context::enter_task(info);
    f();
    drop(_guard);
    shared.quantum.note_complete(me);
}
