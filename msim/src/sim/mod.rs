#![deny(missing_docs)]

pub use self::config::Config;
pub(crate) use self::runtime::context;

#[cfg(feature = "macros")]
#[cfg_attr(docsrs, doc(cfg(feature = "macros")))]
pub use msim_macros::{main, sim_test, test};

pub mod collections;
mod config;
pub mod fs;
pub mod net;
#[cfg_attr(docsrs, doc(cfg(msim)))]
pub mod plugin;
pub mod rand;
#[cfg_attr(docsrs, doc(cfg(msim)))]
pub mod runtime;
pub mod task;
pub mod time;
mod utils;
