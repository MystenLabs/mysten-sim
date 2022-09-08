use std::cell::Cell;
use tracing::info;

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

pub(crate) fn intercepts_enabled() -> bool {
    INTERCEPTS_ENABLED.with(|e| e.get())
}

/// Cache and call a library function via dlsym()
#[macro_export]
macro_rules! maybe_call_sysimpl {
    ($name:ident, [ $($sig:tt)+ ], $($arg:expr),*) => {{

        lazy_static::lazy_static! {
            static ref NEXT_DL_SYM: unsafe extern "C" $($sig)* = unsafe {

                let fn_name = stringify!($name);
                let fn_name_c = std::ffi::CString::new(fn_name).unwrap();

                let ptr = libc::dlsym(libc::RTLD_NEXT, fn_name_c.as_ptr() as _);
                assert!(!ptr.is_null());
                std::mem::transmute(ptr)
            };
        }

        if !crate::sim::intercept::intercepts_enabled() {
            return NEXT_DL_SYM($($arg,)*);
        }
    }};

    ($name:ident, [ $($sig:tt)+ ]) => {
        maybe_call_sysimpl!($name, [ $($sig)* ],)
    }
}
