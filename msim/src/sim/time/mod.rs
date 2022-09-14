//! Utilities for tracking time.
//!
//!

use crate::define_sys_interceptor;
use crate::rand::{GlobalRng, Rng};
use futures::{select_biased, FutureExt};
use naive_timer::Timer;
#[doc(no_inline)]
pub use std::time::Duration;
use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::Waker,
    time::SystemTime,
};

pub mod error;
mod instant;
mod interval;
mod sleep;

pub use self::instant::Instant;
pub use self::interval::{interval, interval_at, Interval, MissedTickBehavior};
pub use self::sleep::{sleep, sleep_until, Sleep};

pub(crate) struct TimeRuntime {
    handle: TimeHandle,
}

impl TimeRuntime {
    pub fn new(rand: &GlobalRng) -> Self {
        // around 2022
        let base_time = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                60 * 60 * 24 * 365 * (2022 - 1970)
                    + rand.with(|rng| rng.gen_range(0..60 * 60 * 24 * 365)),
            );
        let handle = TimeHandle {
            timer: Arc::new(Mutex::new(Timer::default())),
            clock: ClockHandle::new(base_time),
        };
        TimeRuntime { handle }
    }

    pub fn handle(&self) -> &TimeHandle {
        &self.handle
    }

    /// Advances time to the closest timer event. Returns true if succeed.
    pub fn advance_to_next_event(&self) -> bool {
        let mut timer = self.handle.timer.lock().unwrap();
        if let Some(mut time) = timer.next() {
            // WARN: in some platform such as M1 macOS,
            //       let t0: Instant;
            //       let t1: Instant;
            //       t0 + (t1 - t0) < t1 !!
            // we should add eps to make sure 'now >= deadline' and avoid deadlock
            time += Duration::from_nanos(50);

            timer.expire(time);
            self.handle.clock.set_elapsed(time);
            true
        } else {
            false
        }
    }

    /// Advances time.
    pub fn advance(&self, duration: Duration) {
        self.handle.clock.advance(duration);
    }

    #[allow(dead_code)]
    /// Get the current time.
    pub fn now_instant(&self) -> Instant {
        self.handle.now_instant()
    }
}

/// Handle to a shared time source.
#[derive(Clone)]
pub struct TimeHandle {
    timer: Arc<Mutex<Timer>>,
    clock: ClockHandle,
}

impl TimeHandle {
    /// Returns a `TimeHandle` view over the currently running Runtime.
    pub fn current() -> Self {
        crate::context::current(|h| h.time.clone())
    }

    /// Returns a `TimeHandle` view over the currently running Runtime.
    pub fn try_current() -> Option<Self> {
        crate::context::try_current(|h| h.time.clone())
    }

    /// Return the current time.
    pub fn now_instant(&self) -> Instant {
        self.clock.now_instant()
    }

    /// Return the current time.
    pub fn now_time(&self) -> SystemTime {
        self.clock.now_time()
    }

    /// Returns the amount of time elapsed since this handle was created.
    pub fn elapsed(&self) -> Duration {
        self.clock.elapsed()
    }

    /// Waits until `duration` has elapsed.
    pub fn sleep(&self, duration: Duration) -> Sleep {
        self.sleep_until(self.clock.now_instant() + duration)
    }

    /// Waits until `deadline` is reached.
    pub fn sleep_until(&self, deadline: Instant) -> Sleep {
        Sleep {
            handle: self.clone(),
            deadline,
        }
    }

    /// Require a `Future` to complete before the specified duration has elapsed.
    // TODO: make it Send
    pub fn timeout<T: Future>(
        &self,
        duration: Duration,
        future: T,
    ) -> impl Future<Output = Result<T::Output, error::Elapsed>> {
        let timeout = self.sleep(duration);
        async move {
            select_biased! {
                res = future.fuse() => Ok(res),
                _ = timeout.fuse() => Err(error::Elapsed),
            }
        }
    }

    pub(crate) fn add_timer(
        &self,
        deadline: Instant,
        callback: impl FnOnce() + Send + Sync + 'static,
    ) {
        let mut timer = self.timer.lock().unwrap();
        timer.add(deadline - self.clock.base_instant(), |_| callback());
    }

    /// Schedule waker.wake() in the future.
    pub fn wake_at(&self, deadline: Instant, waker: Waker) {
        self.add_timer(deadline, || waker.wake());
    }

    // Get the elapsed time since the beginning of the test run - this should not be exposed to
    // test code.
    pub(crate) fn time_since_clock_base(&self) -> Duration {
        self.clock.time_since_clock_base()
    }
}

/// Supply tokio::time::advance() API (for compilation only - this method
/// is meaningless inside the simulator).
pub async fn advance(_duration: Duration) {
    unimplemented!("cannot advance clock in simulation - use sleep() instead");
}

/// Require a `Future` to complete before the specified duration has elapsed.
pub fn timeout<T: Future>(
    duration: Duration,
    future: T,
) -> impl Future<Output = Result<T::Output, error::Elapsed>> {
    let handle = TimeHandle::current();
    handle.timeout(duration, future)
}

#[derive(Clone)]
struct ClockHandle {
    inner: Arc<Mutex<Clock>>,
}

#[derive(Debug)]
struct Clock {
    /// Time basis for which mock time is derived.
    base_time: std::time::SystemTime,
    base_instant: Instant,
    /// The amount of mock time which has elapsed.
    elapsed_time: Duration,
}

impl ClockHandle {
    const CLOCK_BASE: Duration = Duration::from_secs(86400);

    fn new(base_time: SystemTime) -> Self {
        let base_instant: Instant = unsafe { std::mem::zeroed() };
        let clock = Clock {
            base_time,
            base_instant,
            // Some code subtracts constant durations from Instant::now(), which underflows if the base
            // instant is too small. That code is incorrect but we'll just make life easy anyway by
            // starting the clock with one day of elapsed time.
            elapsed_time: Self::CLOCK_BASE,
        };
        ClockHandle {
            inner: Arc::new(Mutex::new(clock)),
        }
    }

    fn time_since_clock_base(&self) -> Duration {
        self.elapsed() - Self::CLOCK_BASE
    }

    fn set_elapsed(&self, time: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.elapsed_time = time;
    }

    fn elapsed(&self) -> Duration {
        let inner = self.inner.lock().unwrap();
        inner.elapsed_time
    }

    fn advance(&self, duration: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.elapsed_time += duration;
    }

    fn base_instant(&self) -> Instant {
        let inner = self.inner.lock().unwrap();
        inner.base_instant
    }

    fn now_instant(&self) -> Instant {
        let inner = self.inner.lock().unwrap();
        inner.base_instant + inner.elapsed_time
    }

    fn now_time(&self) -> SystemTime {
        let inner = self.inner.lock().unwrap();
        inner.base_time + inner.elapsed_time
    }
}

// ensure that clock functions are not elided by optimizer
pub(crate) fn ensure_clocks() {
    unsafe {
        #[cfg(target_os = "macos")]
        mach_absolute_time();

        #[cfg(target_os = "linux")]
        {
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            clock_gettime(libc::CLOCK_MONOTONIC, &mut ts as *mut libc::timespec);
        }
    }
}

#[cfg(target_os = "macos")]
define_sys_interceptor!(
    fn gettimeofday(tp: *mut libc::timeval, tz: *mut libc::timezone) -> libc::c_int {
        // NOTE: tz should be NULL.
        // macOS: timezone is no longer used; this information is kept outside the kernel.
        if tp.is_null() {
            return 0;
        }
        let time = TimeHandle::current();
        let dur = time
            .now_time()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();
        tp.write(libc::timeval {
            tv_sec: dur.as_secs() as _,
            tv_usec: dur.subsec_micros() as _,
        });
        0
    }
);

#[cfg(target_os = "macos")]
define_sys_interceptor!(
    fn mach_absolute_time() -> u64 {
        #[repr(C)]
        #[derive(Copy, Clone)]
        struct MachTimebaseInfo {
            numer: u32,
            denom: u32,
        }
        type MachTimebaseInfoT = *mut MachTimebaseInfo;

        lazy_static::lazy_static! {
            static ref MACH_TIME_BASE_INFO: MachTimebaseInfo = {
                extern "C" {
                    fn mach_timebase_info(info: MachTimebaseInfoT) -> libc::c_int;
                }

                let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
                unsafe {
                    mach_timebase_info(&mut info as MachTimebaseInfoT);
                }
                assert_ne!(info.numer, 0);
                assert_ne!(info.denom, 0);
                info
            };
        }

        fn mul_div_u64(value: u64, numer: u64, denom: u64) -> u64 {
            let q = value / denom;
            let r = value % denom;
            // Decompose value as (value/denom*denom + value%denom),
            // substitute into (value*numer)/denom and simplify.
            // r < denom, so (denom*numer) is the upper bound of (r*numer)
            q * numer + r * numer / denom
        }

        let elapsed = TimeHandle::current().elapsed();
        let nanos = elapsed.as_nanos().try_into().unwrap();

        // convert nanos back to mach_absolute_time units
        mul_div_u64(
            nanos,
            MACH_TIME_BASE_INFO.denom as u64,
            MACH_TIME_BASE_INFO.numer as u64,
        )
    }
);

#[cfg(target_os = "linux")]
define_sys_interceptor!(
    fn clock_gettime(clock_id: libc::clockid_t, ts: *mut libc::timespec) -> libc::c_int {
        let time = TimeHandle::current();
        match clock_id {
            // used by SystemTime
            libc::CLOCK_REALTIME | libc::CLOCK_REALTIME_COARSE => {
                let dur = time
                    .now_time()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap();
                ts.write(libc::timespec {
                    tv_sec: dur.as_secs() as _,
                    tv_nsec: dur.subsec_nanos() as _,
                });
            }
            // used by Instant
            libc::CLOCK_MONOTONIC | libc::CLOCK_MONOTONIC_RAW | libc::CLOCK_MONOTONIC_COARSE => {
                // Instant is the same layout as timespec on linux
                ts.write(std::mem::transmute(time.now_instant()));
            }

            _ => panic!("unsupported clockid: {}", clock_id),
        }
        0
    }
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;

    #[test]
    fn time() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let t0 = Instant::now();
            let std_t0 = std::time::Instant::now();

            // Verify that times in other threads are not intercepted.
            let std_t1 = std::thread::spawn(|| std::time::Instant::now())
                .join()
                .unwrap();
            assert_ne!(std_t0, std_t1);

            sleep(Duration::from_secs(1)).await;
            assert!(t0.elapsed() >= Duration::from_secs(1));

            sleep_until(t0 + Duration::from_secs(2)).await;
            assert!(t0.elapsed() >= Duration::from_secs(2));

            sleep(Duration::from_secs(20)).await;

            // make sure system clock has been intercepted.
            assert_eq!(std_t0.elapsed(), t0.elapsed());
            assert!(
                timeout(Duration::from_secs(2), sleep(Duration::from_secs(1)))
                    .await
                    .is_ok()
            );
            assert!(
                timeout(Duration::from_secs(1), sleep(Duration::from_secs(2)))
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn system_clock() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            let t0 = Instant::now();
            let s0 = SystemTime::now();

            println!("{:?} {:?}", t0, s0);
        });
    }
}
