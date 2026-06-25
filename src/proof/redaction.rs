use crate::proof::digest_with_key;
use crate::proof::receipt::ProofHmacKey;
use crate::proof::schema::RedactionReport;

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
        let redacted = Self::redact_error_payload(err);
        (
            redacted.chars().take(256).collect(),
            RedactionField::Truncated,
        )
    }

    fn redact_error_payload(err: &str) -> String {
        err.split_whitespace()
            .map(Self::sanitize_error_token)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn sanitize_error_token(token: &str) -> String {
        if Self::looks_like_secret_token(token) {
            "[REDACTED]".to_string()
        } else {
            token.to_string()
        }
    }

    fn looks_like_secret_token(token: &str) -> bool {
        const PATH_PREFIXES: &[&str] = &[
            "/home/", "/users/", "/tmp/", "/var/", "c:\\", "C:\\", "C:/", "c:/",
        ];
        const SECRET_PREFIXES: &[&str] = &[
            "api_key",
            "apikey",
            "authorization",
            "bearer",
            "password",
            "secret",
            "token",
            "raw_memory",
            "sk-",
            "x-api-key",
        ];

        let normalized = token.trim_matches(|c: char| {
            matches!(
                c,
                '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ';' | ',' | '.'
            )
        });
        if normalized.is_empty() {
            return false;
        }

        let lower = normalized.to_ascii_lowercase();
        PATH_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
            || SECRET_PREFIXES.iter().any(|hint| lower.contains(hint))
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
