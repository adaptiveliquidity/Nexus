/// Caller-safe denial categories for security-sensitive request failures.
///
/// These reasons intentionally expose only policy outcomes, not token ids,
/// timestamps, host paths, OS errors, or other server-side details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenialReason {
    /// A capability token is expired.
    TokenExpired,
    /// A capability token was revoked.
    TokenRevoked,
    /// A requested capability is not permitted by the active profile.
    CapabilityNotPermitted,
    /// The requested MCP tool is not in the configured allowlist.
    ToolNotAllowed,
    /// The active profile denied the request.
    ProfileRestriction,
    /// The supplied WASM path is outside the allowed module directories.
    WasmPathDenied,
    /// The supplied WASM path cannot be accessed safely.
    WasmPathInaccessible,
}

impl DenialReason {
    /// Return a stable caller-safe error message for this denial.
    pub fn safe_message(&self) -> &'static str {
        match self {
            Self::TokenExpired => "capability token has expired",
            Self::TokenRevoked => "capability token has been revoked",
            Self::CapabilityNotPermitted => "capability not permitted by active profile",
            Self::ToolNotAllowed => "tool is not in the MCP tool allowlist",
            Self::ProfileRestriction => "request denied by active profile",
            Self::WasmPathDenied => "wasm path is outside allowed module directories",
            Self::WasmPathInaccessible => "wasm path is not accessible",
        }
    }
}
