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
}

impl CapabilityProfileManifest {
    /// Return the capabilities allowed by this manifest.
    pub fn allowed_capabilities(&self) -> &[Capability] {
        &self.capabilities
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
        })
    } else {
        Err(errors)
    }
}

#[derive(Debug, Default)]
struct RawProfile {
    name: Option<String>,
    capabilities: Vec<RawCapability>,
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
    let mut current_capability = None;

    for (index, original_line) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line_without_comment = strip_comment(original_line);
        let line = line_without_comment.trim();

        if line.is_empty() {
            continue;
        }

        if line == "[[capabilities]]" {
            profile.capabilities.push(RawCapability::new(line_number));
            current_capability = Some(profile.capabilities.len() - 1);
            continue;
        }

        if line.starts_with('[') {
            errors.push(ValidationError::Parse {
                line: line_number,
                message: "only [[capabilities]] tables are supported".to_string(),
            });
            continue;
        }

        let Some(separator) = find_unquoted_equals(line) else {
            errors.push(ValidationError::Parse {
                line: line_number,
                message: "expected key = \"value\" assignment".to_string(),
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

        let value = match parse_string_value(line[separator + 1..].trim()) {
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

        match current_capability {
            Some(capability_index) => match key {
                "type" => profile.capabilities[capability_index].capability_type = Some(parsed),
                "path" => profile.capabilities[capability_index].path = Some(parsed),
                "pattern" => profile.capabilities[capability_index].pattern = Some(parsed),
                _ => {}
            },
            None => {
                if key == "name" {
                    profile.name = Some(parsed.value);
                }
            }
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
}
