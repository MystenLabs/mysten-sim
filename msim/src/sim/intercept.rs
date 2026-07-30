use std::cell::Cell;
use tracing::{info, trace};

thread_local! {
    static INTERCEPTS_ENABLED: Cell<bool> = Cell::new(false);
}

// This is called at the beginning of the test thread so that clock calls inside the test are
// deterministic. Other threads (e.g. any thread doing real io) are unaffected.
pub(crate) fn enable_intercepts(e: bool) {
    let cur_thread = std::thread::current().id();
    info!(
        "{} library call intercepts on thread {:?}",
        if e { "enabling" } else { "disabling" },
        cur_thread
    );
    INTERCEPTS_ENABLED.with(|enabled| enabled.set(e))
}

// Quiet variant for blocking-pool threads: their startup runs concurrently with the
// main sim thread, so an info-level log here (with a nondeterministic ThreadId) lands
// at a racy position in otherwise-deterministic log output.
pub(crate) fn enable_intercepts_quiet(e: bool) {
    trace!(
        "{} library call intercepts on thread {:?}",
        if e { "enabling" } else { "disabling" },
        std::thread::current().id()
    );
    INTERCEPTS_ENABLED.with(|enabled| enabled.set(e))
}

pub(crate) fn intercepts_enabled() -> bool {
    INTERCEPTS_ENABLED.with(|e| e.get())
}

/// Enable intercepts (quietly) on the current thread for the lifetime of the returned
/// guard, disabling them again on drop.
///
/// Used by blocking-pool threads: their TLS destructors run at thread teardown, after
/// any runtime-context guard has been dropped. An intercepted syscall there (intercepts
/// enabled but no context) panics inside an `extern "C"` fn, which cannot unwind and
/// aborts the process. Disabling on drop routes those late calls back to the real libc.
pub(crate) fn enable_intercepts_scoped() -> InterceptsGuard {
    enable_intercepts_quiet(true);
    InterceptsGuard(())
}

pub(crate) struct InterceptsGuard(());

impl Drop for InterceptsGuard {
    fn drop(&mut self) {
        enable_intercepts_quiet(false);
    }
}

/// Disable intercepts on the current thread for the lifetime of the returned guard,
/// restoring the previous setting on drop. The inverse of [`enable_intercepts_scoped`].
///
/// This is for narrow teardown windows that must let a specific set of late syscalls
/// (e.g. from a resource's `Drop`) reach the real libc. It intentionally does not make
/// the interceptors globally tolerant of a missing reactor - an intercepted syscall with
/// no simulation context anywhere else is a real bug we want to keep surfacing loudly.
pub(crate) fn disable_intercepts_scoped() -> InterceptsRestoreGuard {
    let previous = intercepts_enabled();
    enable_intercepts_quiet(false);
    InterceptsRestoreGuard(previous)
}

pub(crate) struct InterceptsRestoreGuard(bool);

impl Drop for InterceptsRestoreGuard {
    fn drop(&mut self) {
        enable_intercepts_quiet(self.0);
    }
}

/// Cache and call a library function via dlsym()
#[macro_export]
macro_rules! define_sys_interceptor {

    (fn $name:ident ( $($param:ident : $type:ty),* $(,)? ) -> $ret:ty { $($body:tt)+ }) => {

        #[no_mangle]
        #[inline(never)]
        unsafe extern "C" fn $name ( $($param: $type),* ) -> $ret {
            lazy_static::lazy_static! {
                static ref NEXT_DL_SYM: unsafe extern "C" fn ( $($param: $type),* ) -> $ret = unsafe {

                    // Can't use CString::new because it allocates, and allocators can call system
                    // functions...
                    let fn_name_c = concat!(stringify!($name), "\0");

                    let ptr = libc::dlsym(libc::RTLD_NEXT, fn_name_c.as_ptr() as _);
                    assert!(!ptr.is_null(), "{:?}", fn_name_c);
                    std::mem::transmute(ptr)
                };
            }

            if !crate::sim::intercept::intercepts_enabled() {
                return NEXT_DL_SYM($($param),*);
            }

            $($body)*
        }
    }
}

/// define a function that can be used to bypass a interception (as defined by
/// define_sys_interceptor.
#[macro_export]
macro_rules! define_bypass {
    ($name:ident, fn $cname:ident ( $($param:ident : $type:ty),* $(,)? ) -> $ret:ty) => {
        unsafe fn $name ( $($param: $type),* ) -> $ret {
            lazy_static::lazy_static! {
                static ref NEXT_DL_SYM: unsafe extern "C" fn ( $($param: $type),* ) -> $ret = unsafe {

                    // Can't use CString::new because it allocates, and allocators can call system
                    // functions...
                    let fn_name_c = concat!(stringify!($cname), "\0");

                    let ptr = libc::dlsym(libc::RTLD_NEXT, fn_name_c.as_ptr() as _);
                    assert!(!ptr.is_null());
                    std::mem::transmute(ptr)
                };
            }

            return NEXT_DL_SYM($($param),*);
        }
    }
}
