#[cfg(not(madsim))]
pub use real_tokio::*;

#[cfg(madsim)]
pub use self::sim::*;
#[cfg(madsim)]
mod sim {
    // no mod `runtime`
    pub mod runtime;
    // TODO: simulate `task_local`

    // simulated API
    pub use madsim::task;
    #[cfg(feature = "rt")]
    pub use madsim::task::spawn;
    #[cfg(feature = "time")]
    pub use madsim::time;
    #[cfg(all(feature = "rt", feature = "macros"))]
    pub use madsim::{main, test};

    pub mod net;
    mod unix;

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
    pub use real_tokio::{join, select, try_join};
}
