//! Security Module
//!
//! Provides cryptographic capability tokens and access control.

pub mod capability;
#[cfg(feature = "aeon-memory")]
pub mod negotiator;

pub use capability::{Capability, CapabilityManager, CapabilityToken};
#[cfg(feature = "aeon-memory")]
pub use negotiator::{negotiate_capability_denial, NegotiationOutcome, MAX_NEGOTIATION_ROUNDS};
