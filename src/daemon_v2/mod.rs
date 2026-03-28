pub mod daemon;
pub mod decisions;
pub mod events;
pub mod executor;
pub mod projections;
pub mod rpc;
pub mod scheduler;

pub use daemon::{DaemonV2, DaemonV2Config, DaemonV2ExitStatus};
pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
