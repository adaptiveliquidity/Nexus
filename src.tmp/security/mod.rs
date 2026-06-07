//! Security Module
//! 
//! Provides cryptographic capability tokens and access control.

pub mod capability;

pub use capability::{Capability, CapabilityManager, CapabilityToken};