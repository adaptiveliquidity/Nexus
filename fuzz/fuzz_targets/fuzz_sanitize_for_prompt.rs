//! Fuzz target: arbitrary `&str` inputs to `LLMPolicy::sanitize_for_prompt`
//! must never:
//!   - Panic.
//!   - Return a string longer than `max_input_chars`.
//!   - Contain ASCII control characters other than `\n` and `\t`.
//!
//! This is the property-based companion to the unit tests in
//! `src/hypervisor/llm_policy.rs`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use nexus::hypervisor::{LLMPolicy, LlmBudget, LlmProvider};

fuzz_target!(|data: &str| {
    let policy = LLMPolicy::new(
        LlmProvider::Openai {
            api_key: "fuzz".into(),
            model: "fuzz".into(),
            endpoint: "http://127.0.0.1:0/never".into(),
        },
        LlmBudget {
            max_calls_per_minute: 1000,
            max_input_chars: 128,
            timeout_ms: 1000,
        },
    );
    if let Some(out) = policy.sanitize_for_prompt(data) {
        assert!(out.chars().count() <= 128);
        for c in out.chars() {
            assert!(!c.is_control() || c == '\n' || c == '\t', "leaked control char {c:?}");
        }
    }
});
