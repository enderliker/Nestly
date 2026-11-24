#![forbid(unsafe_code)]
#![doc = "nestly supervisor: module registry, lifecycle, routing, logging, and alerts."]

pub mod alert;
pub mod logging;
pub mod manager;
pub mod process;

pub use manager::{RunMode, Supervisor};
