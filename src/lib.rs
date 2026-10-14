//! aameterd: a metering proxy that implements aaoracled's HTTP contract by
//! forwarding to a real aaoracled, recording every request into SQLite, plus
//! a server for aameterd's own contract that reads those records back live.

pub mod meter;
mod proxy;
mod stats;

pub use meter::Meter;
pub use proxy::router;
pub use stats::router as stats_router;
