use crate::proof::digest_with_key;
use crate::proof::receipt::ProofHmacKey;
use crate::proof::schema::RedactionReport;

const ERROR_SUMMARY_MAX_CHARS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionField {
    HmacOrPlaceholder,
    Truncated,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionPolicy {
    pub hmac_key: ProofHmacKey,
}

impl RedactionPolicy {
    pub fn new(key: ProofHmacKey) -> Self {
        Self { hmac_key: key }
    }

    pub fn redact_path(&self, path: &str) -> (String, RedactionField) {
        let redacted = match &self.hmac_key {
            ProofHmacKey::Disabled => "[PATH_REDACTED]".to_owned(),
            key => digest_with_key(key, path.as_bytes()).value,
        };

        (redacted, RedactionField::HmacOrPlaceholder)
    }

    pub fn redact_token(&self, token: &str) -> (String, RedactionField) {
        // Truncating to a prefix leaks the token's leading characters, which can
        // be enough to identify or brute-force short tokens. Use HMAC (or a
        // static placeholder when HMAC is disabled) to produce a non-reversible,
        // correlation-safe redacted representation — consistent with redact_path.
        let redacted = match &self.hmac_key {
            ProofHmacKey::Disabled => "[TOKEN_REDACTED]".to_owned(),
            key => digest_with_key(key, token.as_bytes()).value,
        };
        (redacted, RedactionField::HmacOrPlaceholder)
    }

    pub fn redact_env_value(&self, _val: &str) -> (String, RedactionField) {
        ("[ENV_REDACTED]".to_owned(), RedactionField::Removed)
    }

    pub fn redact_error(&self, err: &str) -> (String, RedactionField) {
        let (redacted, applied) = self.redact_error_for_field("failure.error_summary", err);
        let field = applied
            .iter()
            .find_map(|(_, field)| (*field == RedactionField::Truncated).then_some(*field))
            .or_else(|| {
                applied
                    .iter()
                    .find_map(|(_, field)| {
                        (*field == RedactionField::HmacOrPlaceholder).then_some(*field)
                    })
                    .or_else(|| {
                        applied.iter().find_map(|(_, field)| {
                            (*field == RedactionField::Removed).then_some(*field)
                        })
                    })
            })
            .unwrap_or(RedactionField::Truncated);

        (redacted, field)
    }

    pub fn redact_error_for_field(
        &self,
        field_name: &str,
        err: &str,
    ) -> (String, Vec<(String, RedactionField)>) {
        let mut applied = Vec::new();
        let (without_forbidden_markers, removed_forbidden_markers) = remove_forbidden_markers(err);
        if removed_forbidden_markers {
            push_redaction(&mut applied, field_name, RedactionField::Removed);
        }

        let mut redact_next_token = false;
        let mut words = Vec::new();
        for word in without_forbidden_markers.split_whitespace() {
            if redact_next_token {
                let (redacted, field) = self.redact_token(word);
                words.push(redacted);
                push_redaction(&mut applied, field_name, field);
                redact_next_token = false;
                continue;
            }

            let lower = word.to_ascii_lowercase();
            if looks_like_bearer_marker(&lower) {
                words.push("[TOKEN_REDACTED]".to_owned());
                push_redaction(&mut applied, field_name, RedactionField::HmacOrPlaceholder);
                redact_next_token = !lower.contains('=')
                    && !lower.contains(':')
                    && lower.trim_matches(|c: char| !c.is_ascii_alphanumeric()) == "bearer";
            } else if looks_like_path(word) {
                let (redacted, field) = self.redact_path(word);
                words.push(redacted);
                push_redaction(&mut applied, field_name, field);
            } else if looks_like_secret_token(word) {
                let (redacted, field) = self.redact_token(word);
                words.push(redacted);
                push_redaction(&mut applied, field_name, field);
            } else {
                words.push(word.to_owned());
            }
        }

        let sanitized = words.join(" ");
        let redacted: String = sanitized.chars().take(ERROR_SUMMARY_MAX_CHARS).collect();
        if sanitized.chars().count() > ERROR_SUMMARY_MAX_CHARS {
            push_redaction(&mut applied, field_name, RedactionField::Truncated);
        }

        (redacted, applied)
    }

    pub fn build_report(&self, applied: Vec<(String, RedactionField)>) -> RedactionReport {
        let mut report = RedactionReport {
            hashed_fields: Vec::new(),
            truncated_fields: Vec::new(),
            removed_fields: Vec::new(),
            hmac_fields: Vec::new(),
        };

        for (field, kind) in applied {
            match kind {
                RedactionField::HmacOrPlaceholder => report.hmac_fields.push(field),
                RedactionField::Truncated => report.truncated_fields.push(field),
                RedactionField::Removed => report.removed_fields.push(field),
            }
        }

        report
    }

    pub fn apply(&self, applied: Vec<(String, RedactionField)>) -> RedactionReport {
        self.build_report(applied)
    }
}

fn push_redaction(applied: &mut Vec<(String, RedactionField)>, field: &str, kind: RedactionField) {
    let entry = (field.to_owned(), kind);
    if !applied.contains(&entry) {
        applied.push(entry);
    }
}

fn remove_forbidden_markers(input: &str) -> (String, bool) {
    [".env", "preview_base64", "BEGIN PRIVATE KEY"]
        .into_iter()
        .fold((input.to_owned(), false), |(current, changed), marker| {
            let (next, marker_changed) =
                replace_ascii_case_insensitive(&current, marker, "[REMOVED]");
            (next, changed || marker_changed)
        })
}

fn replace_ascii_case_insensitive(input: &str, needle: &str, replacement: &str) -> (String, bool) {
    let lower_input = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut cursor = 0;
    let mut changed = false;
    let mut out = String::with_capacity(input.len());

    while let Some(found) = lower_input[cursor..].find(&lower_needle) {
        let start = cursor + found;
        let end = start + needle.len();
        out.push_str(&input[cursor..start]);
        out.push_str(replacement);
        cursor = end;
        changed = true;
    }

    if changed {
        out.push_str(&input[cursor..]);
        (out, true)
    } else {
        (input.to_owned(), false)
    }
}

fn looks_like_bearer_marker(lower_word: &str) -> bool {
    let trimmed = lower_word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    trimmed == "bearer"
        || lower_word.starts_with("bearer=")
        || lower_word.starts_with("bearer:")
        || lower_word.starts_with("authorization:")
        || lower_word.starts_with("authorization=")
}

fn looks_like_secret_token(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    let trimmed = lower.trim_matches(|c: char| {
        !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '=' && c != ':'
    });

    trimmed.starts_with("sk-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("xoxb-")
        || trimmed.contains("token=")
        || trimmed.contains("token:")
        || trimmed.contains("api_key")
        || trimmed.contains("apikey")
        || trimmed.contains("api-key")
        || trimmed.contains("secret=")
        || trimmed.contains("secret:")
        || trimmed.contains("password=")
        || trimmed.contains("password:")
        || trimmed.contains("private_key")
}

fn looks_like_path(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.starts_with('/')
        || lower.contains("/home/")
        || lower.contains("/users/")
        || lower.contains("/.ssh")
        || lower.contains("\\users\\")
        || lower.contains("\\.ssh")
        || is_windows_drive_path(&lower)
}

fn is_windows_drive_path(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}
