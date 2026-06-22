//! Security Module
//!
//! Provides cryptographic capability tokens and access control.

pub mod capability;
pub mod denial;
#[cfg(feature = "aeon-memory")]
pub mod negotiator;

pub use capability::{Capability, CapabilityManager, CapabilityToken};
pub use denial::DenialReason;
#[cfg(feature = "aeon-memory")]
pub use negotiator::{negotiate_capability_denial, NegotiationOutcome, MAX_NEGOTIATION_ROUNDS};
