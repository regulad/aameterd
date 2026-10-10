//! aameterd: a metering proxy that implements aaoracled's HTTP contract by
//! forwarding to a real aaoracled, recording every request into SQLite.

pub mod meter;
mod proxy;

pub use meter::Meter;
pub use proxy::router;
