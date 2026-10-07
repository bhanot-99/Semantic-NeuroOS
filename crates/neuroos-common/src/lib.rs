//! config, paths, process identity, time (UTC ns), logging init, errors

// C1: no crate outside neuroos-shm/neuroos-sandbox may write `unsafe`
// (rules.md §6). This makes that mechanical rather than aspirational.
#![deny(unsafe_code)]

pub mod config;
pub mod logging;
pub mod paths;
pub mod process;
pub mod sync;
pub mod time;

pub use config::{Config, ConfigError, HealthdConfig, TargetConfig, load_config, load_config_from};
pub use logging::init_logging;
pub use process::current_uid;
pub use sync::lock;
pub use time::now_ns;
