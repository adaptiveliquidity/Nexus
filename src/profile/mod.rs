//! Capability profile manifest parsing and validation.
//!
//! Profiles use a deliberately small TOML subset so Nexus can validate simple
//! capability manifests without adding a TOML parser dependency.

use crate::security::Capability;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Validated capability profile manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProfileManifest {
    /// Human-readable profile name from the manifest or file stem.
    pub name: String,
    /// Security capabilities allowed by this profile.
    pub capabilities: Vec<Capability>,
    /// Policy for the MCP tool surface, from the optional `[mcp]` table.
    pub mcp: McpPolicy,
    /// Execution environment policy, from the optional `[execution]` table.
    pub execution: ExecutionPolicy,
}

impl CapabilityProfileManifest {
    /// Return the capabilities allowed by this manifest.
    pub fn allowed_capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    /// Return the MCP-surface policy for this profile.
    pub fn mcp_policy(&self) -> &McpPolicy {
        &self.mcp
    }

    /// Return the execution environment policy for this profile.
    pub fn execution_policy(&self) -> &ExecutionPolicy {
        &self.execution
    }
}

/// Execution environment policy, parsed from the optional `[execution]` table.
///
/// An absent `[execution]` table falls back to permissive defaults so existing
/// profiles keep their current behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecutionPolicy {
    /// Allowed WASM module directories. When non-empty these replace
    /// `NEXUS_MCP_MODULE_DIR`; when empty the env-var fallback applies.
    pub module_dirs: Vec<PathBuf>,
    /// Whether daemon/API callers must present an auth token. When `true` the
    /// daemon refuses to start without `NEXUS_AGENTD_AUTH_TOKEN` configured.
    pub daemon_auth_required: bool,
}

/// Policy for the MCP tool surface, parsed from the optional `[mcp]` table.
///
/// An absent table — or absent individual fields — falls back to permissive
/// defaults so existing capability-only profiles keep their current behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPolicy {
    /// When `Some`, only these MCP tool names may be invoked. `None` leaves all
    /// registered tools callable (no tool-level gating).
    pub tool_allowlist: Option<Vec<String>>,
    /// Whether the snapshot create/rollback tools are permitted.
    pub snapshot_enabled: bool,
    /// Whether the fork-and-race tool is permitted.
    pub fork_enabled: bool,
    /// Whether the nexus_execute_proof tool is permitted.
    pub proof_enabled: bool,
    /// Whether the nexus_execute_wasi tool is permitted.
    pub wasi_enabled: bool,
}

impl Default for McpPolicy {
    fn default() -> Self {
        Self {
            tool_allowlist: None,
            snapshot_enabled: true,
            fork_enabled: true,
            proof_enabled: true,
            wasi_enabled: true,
        }
    }
}

impl McpPolicy {
    /// Whether `tool` may be invoked under this policy's tool allowlist.
    ///
    /// A `None` allowlist permits every tool; an empty allowlist denies all.
    pub fn allows_tool(&self, tool: &str) -> bool {
        match &self.tool_allowlist {
            Some(list) => list.iter().any(|allowed| allowed == tool),
            None => true,
        }
    }
}

/// Validation and parsing errors produced while loading a capability profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// The profile file could not be read.
    Io {
        /// Path that failed to load.
        path: PathBuf,
        /// Underlying I/O error message.
        message: String,
    },
    /// The profile did not match the supported manifest syntax.
    Parse {
        /// 1-based source line number.
        line: usize,
        /// Human-readable parse failure.
        message: String,
    },
    /// The manifest declared no capability entries.
    EmptyCapabilities,
    /// A capability entry was missing its `type` field.
    MissingCapabilityType {
        /// 1-based line where the capability entry starts.
        line: usize,
    },
    /// A capability type is not known by Nexus.
    UnknownCapabilityType {
        /// 1-based line containing the unknown type.
        line: usize,
        /// Type string from the manifest.
        capability_type: String,
    },
    /// A capability entry is missing a required value, such as `path`.
    MissingRequiredField {
        /// 1-based line where the capability entry starts or where the empty
        /// field was found.
        line: usize,
        /// Type string from the manifest.
        capability_type: String,
        /// Missing field name.
        field: &'static str,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::Io { path, message } => {
                write!(f, "failed to read profile {}: {message}", path.display())
            }
            ValidationError::Parse { line, message } => {
                write!(f, "line {line}: {message}")
            }
            ValidationError::EmptyCapabilities => {
                write!(
                    f,
                    "profile must declare at least one [[capabilities]] entry"
                )
            }
            ValidationError::MissingCapabilityType { line } => write!(
                f,
                "capability entry starting at line {line} is missing required field type"
            ),
            ValidationError::UnknownCapabilityType {
                line,
                capability_type,
            } => write!(
                f,
                "line {line}: unknown capability type \"{capability_type}\""
            ),
            ValidationError::MissingRequiredField {
                line,
                capability_type,
                field,
            } => write!(
                f,
                "capability \"{capability_type}\" at line {line} is missing required field {field}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Load and validate a capability profile manifest.
///
/// Supported profile shape:
///
/// ```toml
/// name = "sample"
///
/// [[capabilities]]
/// type = "read_file"
/// path = "/tmp"
/// ```
pub fn load_and_validate(
    path: impl AsRef<std::path::Path>,
) -> Result<CapabilityProfileManifest, Vec<ValidationError>> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|error| {
        vec![ValidationError::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }]
    })?;

    let mut errors = Vec::new();
    let raw = parse_profile(&contents, &mut errors);
    let capabilities = validate_capabilities(&raw.capabilities, &mut errors);

    if errors.is_empty() {
        Ok(CapabilityProfileManifest {
            name: raw
                .name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| profile_name_from_path(path)),
            capabilities,
            mcp: mcp_policy_from_raw(raw.mcp),
            execution: execution_policy_from_raw(raw.execution),
        })
    } else {
        Err(errors)
    }
}

#[derive(Debug, Default)]
struct RawProfile {
    name: Option<String>,
    capabilities: Vec<RawCapability>,
    mcp: Option<RawMcp>,
    execution: Option<RawExecution>,
}

/// Active table while walking the manifest line by line.
enum Section {
    Top,
    Capability(usize),
    Mcp,
    Execution,
}

#[derive(Debug, Default)]
struct RawExecution {
    module_dirs: Option<Vec<String>>,
    daemon_auth_required: Option<bool>,
}

fn parse_execution_assignment(
    exec: &mut RawExecution,
    key: &str,
    rhs: &str,
    line_number: usize,
    errors: &mut Vec<ValidationError>,
) {
    match key {
        "module_dirs" => match parse_string_array(rhs) {
            Ok(values) => exec.module_dirs = Some(values),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        "daemon_auth_required" => match parse_bool_value(rhs) {
            Ok(value) => exec.daemon_auth_required = Some(value),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        // max_token_validity_secs is a future field; skip unknown keys silently
        // for forward-compat, but reject anything not in the known set.
        other => errors.push(ValidationError::Parse {
            line: line_number,
            message: format!("unsupported [execution] key \"{other}\""),
        }),
    }
}

fn execution_policy_from_raw(raw: Option<RawExecution>) -> ExecutionPolicy {
    match raw {
        None => ExecutionPolicy::default(),
        Some(raw) => ExecutionPolicy {
            module_dirs: raw
                .module_dirs
                .unwrap_or_default()
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            daemon_auth_required: raw.daemon_auth_required.unwrap_or(false),
        },
    }
}

#[derive(Debug)]
struct RawMcp {
    tool_allowlist: Option<Vec<String>>,
    snapshot_enabled: Option<bool>,
    fork_enabled: Option<bool>,
    proof_enabled: Option<bool>,
    wasi_enabled: Option<bool>,
}

impl RawMcp {
    fn new() -> Self {
        Self {
            tool_allowlist: None,
            snapshot_enabled: None,
            fork_enabled: None,
            proof_enabled: None,
            wasi_enabled: None,
        }
    }
}

fn mcp_policy_from_raw(raw: Option<RawMcp>) -> McpPolicy {
    match raw {
        None => McpPolicy::default(),
        Some(raw) => McpPolicy {
            tool_allowlist: raw.tool_allowlist,
            snapshot_enabled: raw.snapshot_enabled.unwrap_or(true),
            fork_enabled: raw.fork_enabled.unwrap_or(true),
            proof_enabled: raw.proof_enabled.unwrap_or(true),
            wasi_enabled: raw.wasi_enabled.unwrap_or(true),
        },
    }
}

fn parse_mcp_assignment(
    mcp: &mut RawMcp,
    key: &str,
    rhs: &str,
    line_number: usize,
    errors: &mut Vec<ValidationError>,
) {
    match key {
        "tool_allowlist" => match parse_string_array(rhs) {
            Ok(values) => mcp.tool_allowlist = Some(values),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        "snapshot_enabled" => match parse_bool_value(rhs) {
            Ok(value) => mcp.snapshot_enabled = Some(value),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        "fork_enabled" => match parse_bool_value(rhs) {
            Ok(value) => mcp.fork_enabled = Some(value),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        "proof_enabled" => match parse_bool_value(rhs) {
            Ok(value) => mcp.proof_enabled = Some(value),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        "wasi_enabled" => match parse_bool_value(rhs) {
            Ok(value) => mcp.wasi_enabled = Some(value),
            Err(message) => errors.push(ValidationError::Parse {
                line: line_number,
                message,
            }),
        },
        other => errors.push(ValidationError::Parse {
            line: line_number,
            message: format!("unsupported [mcp] key \"{other}\""),
        }),
    }
}

fn parse_bool_value(value: &str) -> Result<bool, String> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("expected boolean true or false".to_string()),
    }
}

/// Parse a single-line array of quoted strings, e.g. `["a", "b"]`.
///
/// Commas inside string values and multi-line arrays are not supported; tool
/// names do not contain commas so this stays intentionally small.
fn parse_string_array(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(|| "expected an array of quoted strings in [ ... ]".to_string())?;

    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    for element in inner.split(',') {
        let element = element.trim();
        if element.is_empty() {
            return Err("empty array element".to_string());
        }
        items.push(parse_string_value(element)?);
    }
    Ok(items)
}

#[derive(Debug)]
struct RawCapability {
    line: usize,
    capability_type: Option<ParsedValue>,
    path: Option<ParsedValue>,
    pattern: Option<ParsedValue>,
}

#[derive(Debug, Clone)]
struct ParsedValue {
    line: usize,
    value: String,
}

impl RawCapability {
    fn new(line: usize) -> Self {
        Self {
            line,
            capability_type: None,
            path: None,
            pattern: None,
        }
    }
}

fn parse_profile(contents: &str, errors: &mut Vec<ValidationError>) -> RawProfile {
    let mut profile = RawProfile::default();
    let mut section = Section::Top;

    for (index, original_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line_without_comment = strip_comment(original_line);
        let line = line_without_comment.trim();

        if line.is_empty() {
            continue;
        }

        if line == "[[capabilities]]" {
            profile.capabilities.push(RawCapability::new(line_number));
            section = Section::Capability(profile.capabilities.len() - 1);
            continue;
        }

        if line == "[mcp]" {
            profile.mcp.get_or_insert_with(RawMcp::new);
            section = Section::Mcp;
            continue;
        }

        if line == "[execution]" {
            profile.execution.get_or_insert_with(RawExecution::default);
            section = Section::Execution;
            continue;
        }

        if line.starts_with('[') {
            errors.push(ValidationError::Parse {
                line: line_number,
                message: "only [[capabilities]], [mcp], and [execution] tables are supported"
                    .to_string(),
            });
            continue;
        }

        let Some(separator) = find_unquoted_equals(line) else {
            errors.push(ValidationError::Parse {
                line: line_number,
                message: "expected key = value assignment".to_string(),
            });
            continue;
        };

        let key = line[..separator].trim();
        if key.is_empty() {
            errors.push(ValidationError::Parse {
                line: line_number,
                message: "expected key before =".to_string(),
            });
            continue;
        }

        let rhs = line[separator + 1..].trim();

        if let Section::Mcp = section {
            let mcp = profile.mcp.get_or_insert_with(RawMcp::new);
            parse_mcp_assignment(mcp, key, rhs, line_number, errors);
            continue;
        }

        if let Section::Execution = section {
            let exec = profile.execution.get_or_insert_with(RawExecution::default);
            parse_execution_assignment(exec, key, rhs, line_number, errors);
            continue;
        }

        let value = match parse_string_value(rhs) {
            Ok(value) => value,
            Err(message) => {
                errors.push(ValidationError::Parse {
                    line: line_number,
                    message,
                });
                continue;
            }
        };

        let parsed = ParsedValue {
            line: line_number,
            value,
        };

        match section {
            Section::Capability(capability_index) => match key {
                "type" => profile.capabilities[capability_index].capability_type = Some(parsed),
                "path" => profile.capabilities[capability_index].path = Some(parsed),
                "pattern" => profile.capabilities[capability_index].pattern = Some(parsed),
                _ => {}
            },
            Section::Top => {
                if key == "name" {
                    profile.name = Some(parsed.value);
                }
            }
            Section::Mcp | Section::Execution => unreachable!("handled above"),
        }
    }

    profile
}

fn validate_capabilities(
    raw_capabilities: &[RawCapability],
    errors: &mut Vec<ValidationError>,
) -> Vec<Capability> {
    if raw_capabilities.is_empty() {
        errors.push(ValidationError::EmptyCapabilities);
        return Vec::new();
    }

    let mut capabilities = Vec::with_capacity(raw_capabilities.len());

    for raw in raw_capabilities {
        let Some(capability_type) = &raw.capability_type else {
            errors.push(ValidationError::MissingCapabilityType { line: raw.line });
            continue;
        };

        let original_type = capability_type.value.trim();
        let normalized_type = original_type.to_ascii_lowercase().replace('-', "_");
        let capability = match normalized_type.as_str() {
            "read_file" => {
                required_path(raw, original_type, "path", errors).map(Capability::ReadFile)
            }
            "write_file" => {
                required_path(raw, original_type, "path", errors).map(Capability::WriteFile)
            }
            "list_dir" | "list_directory" => {
                required_path(raw, original_type, "path", errors).map(Capability::ListDirectory)
            }
            "http_get" => required_pattern(raw, original_type, errors).map(Capability::HttpGet),
            "http_post" => required_pattern(raw, original_type, errors).map(Capability::HttpPost),
            "execute" | "execute_binary" => {
                required_path(raw, original_type, "path", errors).map(Capability::ExecuteBinary)
            }
            "mount_tmpfs" => {
                required_path(raw, original_type, "path", errors).map(Capability::MountTmpfs)
            }
            "all" => Some(Capability::All),
            "none" => Some(Capability::None),
            _ => {
                errors.push(ValidationError::UnknownCapabilityType {
                    line: capability_type.line,
                    capability_type: original_type.to_string(),
                });
                None
            }
        };

        if let Some(capability) = capability {
            capabilities.push(capability);
        }
    }

    capabilities
}

fn required_path(
    raw: &RawCapability,
    capability_type: &str,
    field: &'static str,
    errors: &mut Vec<ValidationError>,
) -> Option<PathBuf> {
    required_value(raw, capability_type, field, raw.path.as_ref(), errors).map(PathBuf::from)
}

fn required_pattern(
    raw: &RawCapability,
    capability_type: &str,
    errors: &mut Vec<ValidationError>,
) -> Option<String> {
    let value = raw.pattern.as_ref().or(raw.path.as_ref());
    required_value(raw, capability_type, "pattern", value, errors)
}

fn required_value(
    raw: &RawCapability,
    capability_type: &str,
    field: &'static str,
    value: Option<&ParsedValue>,
    errors: &mut Vec<ValidationError>,
) -> Option<String> {
    let Some(value) = value else {
        errors.push(ValidationError::MissingRequiredField {
            line: raw.line,
            capability_type: capability_type.to_string(),
            field,
        });
        return None;
    };

    let parsed_value = value;
    let value = parsed_value.value.trim();
    if value.is_empty() {
        errors.push(ValidationError::MissingRequiredField {
            line: parsed_value.line,
            capability_type: capability_type.to_string(),
            field,
        });
        None
    } else {
        Some(value.to_string())
    }
}

fn strip_comment(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut quote = None;
    let mut escaped = false;

    for character in line.chars() {
        match quote {
            Some('"') if escaped => {
                escaped = false;
                result.push(character);
            }
            Some('"') if character == '\\' => {
                escaped = true;
                result.push(character);
            }
            Some(active_quote) if character == active_quote => {
                quote = None;
                result.push(character);
            }
            Some(_) => result.push(character),
            None if character == '#' => break,
            None if character == '"' || character == '\'' => {
                quote = Some(character);
                result.push(character);
            }
            None => result.push(character),
        }
    }

    result
}

fn find_unquoted_equals(line: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if character == '\\' => escaped = true,
            Some(active_quote) if character == active_quote => quote = None,
            Some(_) => {}
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == '=' => return Some(index),
            None => {}
        }
    }

    None
}

fn parse_string_value(value: &str) -> Result<String, String> {
    let Some(quote) = value.chars().next() else {
        return Err("expected quoted string value".to_string());
    };

    match quote {
        '"' => parse_basic_string(value),
        '\'' => parse_literal_string(value),
        _ => Err("expected quoted string value".to_string()),
    }
}

fn parse_basic_string(value: &str) -> Result<String, String> {
    let mut parsed = String::new();
    let mut escaped = false;

    for (relative_index, character) in value[1..].char_indices() {
        let absolute_index = relative_index + 1;
        if escaped {
            match character {
                '"' => parsed.push('"'),
                '\\' => parsed.push('\\'),
                'n' => parsed.push('\n'),
                'r' => parsed.push('\r'),
                't' => parsed.push('\t'),
                other => {
                    return Err(format!("unsupported escape sequence \\{other}"));
                }
            }
            escaped = false;
            continue;
        }

        match character {
            '\\' => escaped = true,
            '"' => {
                let rest = &value[absolute_index + character.len_utf8()..];
                if rest.trim().is_empty() {
                    return Ok(parsed);
                }
                return Err("unexpected content after string value".to_string());
            }
            other => parsed.push(other),
        }
    }

    Err("unterminated string value".to_string())
}

fn parse_literal_string(value: &str) -> Result<String, String> {
    let rest = &value[1..];
    let Some(end_index) = rest.find('\'') else {
        return Err("unterminated string value".to_string());
    };

    let trailing = &rest[end_index + 1..];
    if trailing.trim().is_empty() {
        Ok(rest[..end_index].to_string())
    } else {
        Err("unexpected content after string value".to_string())
    }
}

fn profile_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("profile")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type CapabilityProfile = CapabilityProfileManifest;

    impl CapabilityProfileManifest {
        fn from_str_for_test(contents: &str) -> Result<Self, Vec<ValidationError>> {
            let mut errors = Vec::new();
            let raw = parse_profile(contents, &mut errors);
            let capabilities = if raw.capabilities.is_empty() {
                Vec::new()
            } else {
                validate_capabilities(&raw.capabilities, &mut errors)
            };

            if errors.is_empty() {
                Ok(CapabilityProfileManifest {
                    name: raw.name.unwrap_or_else(|| "test-profile".to_string()),
                    mcp: mcp_policy_from_raw(raw.mcp),
                    execution: execution_policy_from_raw(raw.execution),
                    capabilities,
                })
            } else {
                Err(errors)
            }
        }
    }

    static NEXT_PROFILE_ID: AtomicUsize = AtomicUsize::new(0);

    fn write_profile(contents: &str) -> PathBuf {
        let id = NEXT_PROFILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nexus-profile-test-{}-{id}.toml",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("write profile");
        path
    }

    #[test]
    fn loads_read_file_capability() {
        let path = write_profile(
            r#"
name = "sample"

[[capabilities]]
type = "read_file"
path = "/tmp"
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        assert_eq!(manifest.name, "sample");
        assert_eq!(
            manifest.allowed_capabilities(),
            &[Capability::ReadFile(PathBuf::from("/tmp"))]
        );
    }

    #[test]
    fn validates_unknown_type_and_missing_path() {
        let path = write_profile(
            r#"
name = "bad"

[[capabilities]]
type = "bogus"

[[capabilities]]
type = "write_file"
"#,
        );

        let errors = load_and_validate(&path).expect_err("invalid profile");
        std::fs::remove_file(path).ok();

        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::UnknownCapabilityType {
                capability_type,
                ..
            } if capability_type == "bogus"
        )));
        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::MissingRequiredField {
                capability_type,
                field: "path",
                ..
            } if capability_type == "write_file"
        )));
    }

    #[test]
    fn rejects_empty_capability_list() {
        let path = write_profile(r#"name = "empty""#);

        let errors = load_and_validate(&path).expect_err("invalid profile");
        std::fs::remove_file(path).ok();

        assert!(errors
            .iter()
            .any(|error| matches!(error, ValidationError::EmptyCapabilities)));
    }

    #[test]
    fn accepts_http_pattern_from_path_field() {
        let path = write_profile(
            r#"
name = "network"

[[capabilities]]
type = "http_get"
path = "https://example.com/*"
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        assert_eq!(
            manifest.allowed_capabilities(),
            &[Capability::HttpGet("https://example.com/*".to_string())]
        );
    }

    #[test]
    fn parses_mcp_policy_table() {
        let path = write_profile(
            r#"
name = "gated"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
tool_allowlist = ["nexus_execute", "nexus_execute_wasi"]
snapshot_enabled = false
fork_enabled = true
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        let mcp = manifest.mcp_policy();
        assert_eq!(
            mcp.tool_allowlist,
            Some(vec![
                "nexus_execute".to_string(),
                "nexus_execute_wasi".to_string(),
            ])
        );
        assert!(!mcp.snapshot_enabled);
        assert!(mcp.fork_enabled);
        assert!(mcp.allows_tool("nexus_execute"));
        assert!(!mcp.allows_tool("nexus_fork_and_race"));
    }

    #[test]
    fn mcp_policy_defaults_to_permissive_when_absent() {
        let path = write_profile(
            r#"
name = "no-mcp"

[[capabilities]]
type = "read_file"
path = "/tmp"
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        let mcp = manifest.mcp_policy();
        assert_eq!(mcp, &McpPolicy::default());
        assert!(mcp.snapshot_enabled);
        assert!(mcp.fork_enabled);
        assert!(mcp.tool_allowlist.is_none());
        assert!(mcp.allows_tool("anything"));
    }

    #[test]
    fn mcp_policy_partial_table_keeps_field_defaults() {
        let path = write_profile(
            r#"
name = "partial"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
fork_enabled = false
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        let mcp = manifest.mcp_policy();
        assert!(mcp.snapshot_enabled);
        assert!(!mcp.fork_enabled);
        assert!(mcp.tool_allowlist.is_none());
    }

    #[test]
    fn parse_proof_disabled() {
        let path = write_profile(
            r#"
name = "no-proof"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
proof_enabled = false
"#,
        );
        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();
        assert!(!manifest.mcp_policy().proof_enabled);
    }

    #[test]
    fn default_proof_enabled_is_true() {
        let mcp = McpPolicy::default();
        assert!(mcp.proof_enabled);
    }

    #[test]
    fn parse_wasi_disabled() {
        let input = "[mcp]\nwasi_enabled = false";
        let profile = CapabilityProfile::from_str_for_test(input).unwrap();
        assert!(!profile.mcp_policy().wasi_enabled);
    }

    #[test]
    fn default_wasi_enabled_is_true() {
        assert!(McpPolicy::default().wasi_enabled);
    }

    #[test]
    fn mcp_empty_tool_allowlist_denies_all() {
        let path = write_profile(
            r#"
name = "empty-allow"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
tool_allowlist = []
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        let mcp = manifest.mcp_policy();
        assert!(mcp
            .tool_allowlist
            .as_ref()
            .is_some_and(|list| list.is_empty()));
        assert!(!mcp.allows_tool("nexus_execute"));
    }

    #[test]
    fn mcp_rejects_unknown_key() {
        let path = write_profile(
            r#"
name = "bad-mcp"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
snapshots_on = true
"#,
        );

        let errors = load_and_validate(&path).expect_err("invalid profile");
        std::fs::remove_file(path).ok();

        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::Parse { message, .. } if message.contains("unsupported [mcp] key")
        )));
    }

    #[test]
    fn mcp_rejects_non_boolean_flag() {
        let path = write_profile(
            r#"
name = "bad-bool"

[[capabilities]]
type = "read_file"
path = "/tmp"

[mcp]
snapshot_enabled = "yes"
"#,
        );

        let errors = load_and_validate(&path).expect_err("invalid profile");
        std::fs::remove_file(path).ok();

        assert!(errors.iter().any(|error| matches!(
            error,
            ValidationError::Parse { message, .. } if message.contains("boolean")
        )));
    }

    #[test]
    fn parses_execution_module_dirs() {
        let path = write_profile(
            r#"
name = "module-dirs"

[[capabilities]]
type = "read_file"
path = "/tmp"

[execution]
module_dirs = ["/srv/nexus/modules", "/opt/modules"]
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        assert_eq!(
            manifest.execution_policy().module_dirs,
            vec![
                std::path::PathBuf::from("/srv/nexus/modules"),
                std::path::PathBuf::from("/opt/modules"),
            ]
        );
        assert!(!manifest.execution_policy().daemon_auth_required);
    }

    #[test]
    fn parses_execution_daemon_auth_required() {
        let path = write_profile(
            r#"
name = "auth-required"

[[capabilities]]
type = "read_file"
path = "/tmp"

[execution]
daemon_auth_required = true
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        assert!(manifest.execution_policy().daemon_auth_required);
        assert!(manifest.execution_policy().module_dirs.is_empty());
    }

    #[test]
    fn execution_defaults_to_permissive_when_absent() {
        let path = write_profile(
            r#"
name = "no-execution"

[[capabilities]]
type = "read_file"
path = "/tmp"
"#,
        );

        let manifest = load_and_validate(&path).expect("valid profile");
        std::fs::remove_file(path).ok();

        assert!(manifest.execution_policy().module_dirs.is_empty());
        assert!(!manifest.execution_policy().daemon_auth_required);
    }

    #[test]
    fn execution_rejects_unknown_key() {
        let path = write_profile(
            r#"
name = "bad-exec"

[[capabilities]]
type = "read_file"
path = "/tmp"

[execution]
unknown_field = "oops"
"#,
        );

        let errors = load_and_validate(&path).expect_err("should reject unknown execution key");
        std::fs::remove_file(path).ok();

        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::Parse { message, .. } if message.contains("unsupported [execution] key")
        )));
    }
}
