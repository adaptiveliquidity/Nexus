//! Security Module
//!
//! Provides cryptographic capability tokens and access control.

pub mod capability;
pub mod denial;
pub mod egress;
#[cfg(feature = "aeon-memory")]
pub mod negotiator;
pub mod url_guard;

pub use capability::{Capability, CapabilityManager, CapabilityToken};
pub use egress::EgressPolicy;
pub use denial::DenialReason;
#[cfg(feature = "aeon-memory")]
pub use negotiator::{negotiate_capability_denial, NegotiationOutcome, MAX_NEGOTIATION_ROUNDS};
pub use url_guard::{is_blocked_ip, validate_http_capability_pattern, validate_resolved_url};
