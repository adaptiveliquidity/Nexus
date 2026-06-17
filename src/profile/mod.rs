//! Capability profile manifest parsing and validation.
//!
//! This module is intentionally read-only: it validates declarative security
//! posture without issuing tokens or changing runtime execution behavior.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::security::Capability;

/// Maximum token validity allowed by a capability profile, in seconds.
pub const MAX_PROFILE_TOKEN_VALIDITY_SECS: u64 = 86_400;

/// Top-level capability profile manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityProfileManifest {
    pub profile: ProfileSection,
    pub execution: ExecutionSection,
    pub capabilities: Vec<CapabilityEntry>,
    pub mcp: Option<McpSection>,
}

/// Human-readable profile metadata.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSection {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
}

/// Execution posture captured by the manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSection {
    pub module_dirs: Vec<PathBuf>,
    pub daemon_auth_required: bool,
    pub max_token_validity_secs: Option<u64>,
}

/// One requested capability grant.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEntry {
    pub scope: String,
    pub path: Option<PathBuf>,
    pub url: Option<String>,
}

impl CapabilityEntry {
    /// Convert this manifest entry to the existing runtime capability model.
    ///
    /// The returned value is not issued or applied by this module; conversion is
    /// only used to validate that profile scope strings match runtime types.
    pub fn to_capability(&self) -> Result<Capability, ValidationError> {
        match self.scope.as_str() {
            "ReadFile" => Ok(Capability::ReadFile(self.absolute_path()?)),
            "WriteFile" => Ok(Capability::WriteFile(self.absolute_path()?)),
            "ListDirectory" => Ok(Capability::ListDirectory(self.absolute_path()?)),
            "HttpGet" => Ok(Capability::HttpGet(self.url_argument()?)),
            "HttpPost" => Ok(Capability::HttpPost(self.url_argument()?)),
            "ExecuteBinary" => Ok(Capability::ExecuteBinary(self.absolute_path()?)),
            "MountTmpfs" => Ok(Capability::MountTmpfs(self.absolute_path()?)),
            "None" => Ok(Capability::None),
            "All" => Err(ValidationError::AllCapabilityForbidden),
            other => Err(ValidationError::UnknownScope(other.to_string())),
        }
    }

    fn absolute_path(&self) -> Result<PathBuf, ValidationError> {
        let Some(path) = self.path.as_ref() else {
            return Err(ValidationError::NonAbsolutePath(PathBuf::from("<missing>")));
        };
        if path.is_absolute() {
            Ok(path.clone())
        } else {
            Err(ValidationError::NonAbsolutePath(path.clone()))
        }
    }

    fn url_argument(&self) -> Result<String, ValidationError> {
        self.url
            .clone()
            .ok_or_else(|| ValidationError::MissingCapabilityArgument {
                scope: self.scope.clone(),
                field: "url",
            })
    }
}

/// Optional MCP tool posture captured by the manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    pub tool_allowlist: Vec<String>,
    pub snapshot_enabled: bool,
    pub fork_enabled: bool,
}

/// Manifest validation failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("execution.module_dirs must not be empty when execution.daemon_auth_required=false")]
    EmptyModuleDir,
    #[error("Capability::All is forbidden in capability profile manifests")]
    AllCapabilityForbidden,
    #[error("path must be absolute: {0}")]
    NonAbsolutePath(PathBuf),
    #[error("execution.max_token_validity_secs {0} exceeds the profile ceiling of 86400 seconds")]
    TokenValidityTooLong(u64),
    #[error("unknown capability scope: {0}")]
    UnknownScope(String),
    #[error("capability scope {scope} requires `{field}`")]
    MissingCapabilityArgument { scope: String, field: &'static str },
}

/// Validate a parsed manifest.
pub fn validate(manifest: &CapabilityProfileManifest) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    if !manifest.execution.daemon_auth_required && manifest.execution.module_dirs.is_empty() {
        errors.push(ValidationError::EmptyModuleDir);
    }

    for module_dir in &manifest.execution.module_dirs {
        if !module_dir.is_absolute() {
            errors.push(ValidationError::NonAbsolutePath(module_dir.clone()));
        }
    }

    if let Some(max_secs) = manifest.execution.max_token_validity_secs {
        if max_secs > MAX_PROFILE_TOKEN_VALIDITY_SECS {
            errors.push(ValidationError::TokenValidityTooLong(max_secs));
        }
    }

    for capability in &manifest.capabilities {
        if let Err(error) = capability.to_capability() {
            errors.push(error);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Load a TOML profile from disk and validate it.
pub fn load_and_validate(path: &Path) -> anyhow::Result<CapabilityProfileManifest> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read profile '{}'", path.display()))?;
    let manifest: CapabilityProfileManifest = toml::from_str(&raw)
        .with_context(|| format!("failed to parse profile TOML '{}'", path.display()))?;

    if let Err(errors) = validate(&manifest) {
        let messages = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("profile validation failed:\n{messages}");
    }

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> CapabilityProfileManifest {
        toml::from_str(raw).unwrap()
    }

    fn valid_profile() -> &'static str {
        r#"
[profile]
name = "local-secure"
version = "0.1.0"
description = "Local MCP profile with narrow capabilities"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = false
max_token_validity_secs = 3600

[[capabilities]]
scope = "ReadFile"
path = "/srv/nexus/data"

[[capabilities]]
scope = "HttpGet"
url = "https://api.example.com/v1/*"

[[capabilities]]
scope = "None"

[mcp]
tool_allowlist = ["nexus_execute", "nexus_execute_wasi", "nexus_issue_token"]
snapshot_enabled = true
fork_enabled = false
"#
    }

    #[test]
    fn valid_profile_parses_and_validates() {
        let manifest = parse(valid_profile());

        assert!(validate(&manifest).is_ok());
        assert_eq!(manifest.profile.name, "local-secure");
        assert_eq!(
            manifest.capabilities[0].to_capability().unwrap(),
            Capability::ReadFile(PathBuf::from("/srv/nexus/data"))
        );
        assert_eq!(
            manifest.capabilities[1].to_capability().unwrap(),
            Capability::HttpGet("https://api.example.com/v1/*".to_string())
        );
        assert_eq!(
            manifest.capabilities[2].to_capability().unwrap(),
            Capability::None
        );
    }

    #[test]
    fn all_capability_is_rejected() {
        let manifest = parse(
            r#"
[profile]
name = "bad"
version = "0.1.0"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = true

[[capabilities]]
scope = "All"
"#,
        );

        let errors = validate(&manifest).unwrap_err();
        assert!(errors.contains(&ValidationError::AllCapabilityForbidden));
    }

    #[test]
    fn relative_path_is_rejected() {
        let manifest = parse(
            r#"
[profile]
name = "bad"
version = "0.1.0"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = true

[[capabilities]]
scope = "ReadFile"
path = "relative/data"
"#,
        );

        let errors = validate(&manifest).unwrap_err();
        assert!(
            errors.contains(&ValidationError::NonAbsolutePath(PathBuf::from(
                "relative/data"
            )))
        );
    }

    #[test]
    fn token_validity_ceiling() {
        let manifest = parse(
            r#"
[profile]
name = "bad"
version = "0.1.0"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = true
max_token_validity_secs = 90000

[[capabilities]]
scope = "None"
"#,
        );

        let errors = validate(&manifest).unwrap_err();
        assert!(errors.contains(&ValidationError::TokenValidityTooLong(90_000)));
    }

    #[test]
    fn empty_module_dirs_without_daemon_auth() {
        let manifest = parse(
            r#"
[profile]
name = "bad"
version = "0.1.0"

[execution]
module_dirs = []
daemon_auth_required = false

[[capabilities]]
scope = "None"
"#,
        );

        let errors = validate(&manifest).unwrap_err();
        assert!(errors.contains(&ValidationError::EmptyModuleDir));
    }

    #[test]
    fn http_capability_requires_url() {
        let manifest = parse(
            r#"
[profile]
name = "bad"
version = "0.1.0"

[execution]
module_dirs = ["/srv/nexus/modules"]
daemon_auth_required = true

[[capabilities]]
scope = "HttpGet"
"#,
        );

        let errors = validate(&manifest).unwrap_err();
        assert!(
            errors.contains(&ValidationError::MissingCapabilityArgument {
                scope: "HttpGet".to_string(),
                field: "url",
            })
        );
    }
}
