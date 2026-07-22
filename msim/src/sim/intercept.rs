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
