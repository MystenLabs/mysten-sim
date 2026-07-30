// The simulator API only exists under `--cfg msim`; without it this file compiles to an
// empty crate rather than failing to resolve `msim::runtime`.
#![cfg(msim)]

//! Regression tests for futures dropped during runtime teardown.
//!
//! After `block_on` returns, the runtime context is gone but intercepts are still enabled
//! on the test thread (`Runtime::with_seed_and_config` turns them on and never turns them
//! off). Anything still holding a future at that point drops it in that state, and the
//! future's Drop impls can make intercepted syscalls - `close` on a file or socket routes
//! through `plugin::simulator()`/`plugin::node()`, both of which panic with "there is no
//! reactor running". The panic happens inside an `extern "C"` fn, so it aborts the process
//! instead of failing the test.
//!
//! Each test below parks a future owning a `File` somewhere the executor must tear down.
//! An abort here is a real failure even though it cannot be caught - run under nextest,
//! which gives each test its own process.

use msim::runtime::{Handle, Runtime};
use msim::time::sleep;
use std::fs::File;
use std::time::Duration;

fn open_fd() -> File {
    File::open("/dev/null").expect("open /dev/null")
}

/// Baseline: dropping the file inside the runtime, with a context entered, is fine. If
/// this aborts, the harness is broken rather than the runtime.
#[test]
fn drop_inside_runtime() {
    let rt = Runtime::new();
    rt.block_on(async {
        let f = open_fd();
        sleep(Duration::from_millis(1)).await;
        drop(f);
    });
}

/// A runnable parked in `Node::paused` by `pause()` is dropped when the node map is torn
/// down, which happens after the context is gone. Aborted before `Executor::drop` learned
/// to drain the map with intercepts disabled.
#[test]
fn paused_runnable_dropped_at_teardown() {
    let rt = Runtime::new();
    let node = rt.create_node().build();
    let node_id = node.id();
    node.spawn(async move {
        let _f = open_fd();
        loop {
            sleep(Duration::from_millis(1)).await;
        }
    });
    rt.block_on(async move {
        sleep(Duration::from_millis(10)).await;
        Handle::current().pause(node_id);
        // let the task's timer fire while paused, parking its runnable
        sleep(Duration::from_millis(50)).await;
    });
    drop(rt);
}

/// A task woken by the main future's last action is still drained by `run_all_ready`,
/// because the main task itself runs inside that drain loop. Guards the ordering in
/// `Executor::block_on` that makes this true.
#[test]
fn task_woken_as_main_future_completes() {
    let rt = Runtime::new();
    let node = rt.create_node().build();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    node.spawn(async move {
        let _f = open_fd();
        let _ = rx.await;
    });
    rt.block_on(async move {
        sleep(Duration::from_millis(10)).await;
        // wake it and return without yielding again
        let _ = tx.send(());
    });
    drop(rt);
}

/// Jobs enqueued on the blocking pool but never dequeued are dropped by `PoolShared::drop`,
/// from `Executor::drop`, after the context is gone. This is the case that showed up as
/// ~38 `sui-adapter-transactional-tests` aborting on a RocksDB handle's `close()`; the fd
/// here stands in for that handle. The main future enqueues and returns in the same poll,
/// so `block_on` exits before any wake round can dequeue them.
#[test]
fn unrun_blocking_job_dropped_at_teardown() {
    let rt = Runtime::new();
    rt.block_on(async {
        for _ in 0..64 {
            let f = open_fd();
            let handle = msim::task::spawn_blocking(move || drop(f));
            // keep the job queued rather than cancelling it via the handle
            std::mem::forget(handle);
        }
    });
    drop(rt);
}

/// `Executor::drop` shuts the blocking pool down before dropping the ready queue, so a
/// pool thread joining at that moment can wake an async waiter. Guards that the waiter's
/// runnable does not end up dropped context-less.
#[test]
fn waiter_woken_by_blocking_pool_shutdown() {
    let rt = Runtime::new();
    let node = rt.create_node().build();
    node.spawn(async move {
        let _f = open_fd();
        let h = msim::task::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
        });
        let _ = h.await;
    });
    rt.block_on(async {
        sleep(Duration::from_millis(1)).await;
    });
    drop(rt);
}
