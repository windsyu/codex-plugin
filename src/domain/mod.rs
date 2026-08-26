//! Pure Observer business rules. This layer performs no filesystem, network,
//! database, clock, or process I/O and never depends on application layers.

pub mod classify;
pub mod identity;
pub mod live;
pub mod model;
pub mod normalize;
pub mod project;
pub mod redact;
