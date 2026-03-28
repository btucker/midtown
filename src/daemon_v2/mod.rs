pub mod events;
pub mod projections;
pub mod rpc;

pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
