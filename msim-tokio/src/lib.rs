#[cfg(not(msim))]
pub use real_tokio::*;

#[cfg(msim)]
pub use self::sim::*;

#[cfg(msim)]
mod sim {
    // no mod `runtime`
    pub mod runtime;
    // TODO: simulate `task_local`

    // simulated API
    pub use msim::task;
    #[cfg(feature = "rt")]
    pub use msim::task::spawn;
    #[cfg(feature = "time")]
    pub use msim::time;
    #[cfg(all(feature = "rt", feature = "macros"))]
    pub use msim::{sim_test, test};

    pub mod net;
    mod udp;
    pub mod unix;

    // not simulated API
    // TODO: simulate `fs`
    #[cfg(feature = "fs")]
    pub use real_tokio::fs;
    #[cfg(feature = "process")]
    pub use real_tokio::process;
    #[cfg(feature = "signal")]
    pub use real_tokio::signal;
    #[cfg(feature = "sync")]
    pub use real_tokio::sync;
    pub use real_tokio::{io, pin};
    #[cfg(feature = "macros")]
    pub use real_tokio::{join, main, select, try_join};
}

#[cfg(msim)]
mod poller;
