#![cfg(feature = "aeon-memory")]

use crate::aeon::AeonMemoryClient;
use crate::security::{Capability, CapabilityManager, CapabilityToken};

/// Hard cap for denial negotiation attempts.
pub const MAX_NEGOTIATION_ROUNDS: usize = 2;

/// Successful capability-denial negotiation result.
#[derive(Debug, Clone, PartialEq)]
pub struct NegotiationOutcome {
    pub narrowed_capabilities: Vec<Capability>,
    pub rounds: u32,
    pub evidence_hits: Vec<crate::aeon::MemoryHit>,
}

/// Attempt to replace a denied request with an authorized strict subset.
pub async fn negotiate_capability_denial(
    original_required: &[Capability],
    caller_tokens: &[CapabilityToken],
    manager: &CapabilityManager,
    memory: &AeonMemoryClient,
) -> Option<NegotiationOutcome> {
    negotiate_capability_denial_with_authorizer(original_required, memory, |narrowed| {
        manager.authorize(caller_tokens, narrowed).is_ok()
    })
    .await
}

pub(crate) async fn negotiate_capability_denial_with_authorizer(
    original_required: &[Capability],
    memory: &AeonMemoryClient,
    mut authorize: impl FnMut(&[Capability]) -> bool,
) -> Option<NegotiationOutcome> {
    if original_required.is_empty() {
        return None;
    }

    let mut previous_attempt = original_required.to_vec();

    for round in 1..=MAX_NEGOTIATION_ROUNDS {
        let query = negotiation_query(original_required, &previous_attempt, round);
        let hits = memory.search(&query, original_required.len().max(1)).await;
        let candidates = candidates_from_hits(original_required, &hits);
        let narrowed = strict_intersection(original_required, &candidates);

        debug_assert!(
            narrowed
                .iter()
                .all(|capability| original_required.contains(capability)),
            "negotiated capabilities must be drawn only from the original requirement set"
        );
        debug_assert!(
            narrowed.len() < original_required.len(),
            "negotiated capabilities must strictly narrow the original requirement set"
        );

        if narrowed.is_empty() {
            continue;
        }
        if narrowed == previous_attempt {
            continue;
        }

        previous_attempt = narrowed.clone();
        if authorize(&narrowed) {
            return Some(NegotiationOutcome {
                narrowed_capabilities: narrowed,
                rounds: round as u32,
                evidence_hits: hits,
            });
        }
    }

    None
}

fn negotiation_query(
    original_required: &[Capability],
    previous_attempt: &[Capability],
    round: usize,
) -> String {
    let original = original_required
        .iter()
        .map(Capability::description)
        .collect::<Vec<_>>()
        .join(", ");
    let previous = previous_attempt
        .iter()
        .map(Capability::description)
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "capability denial negotiation round {round}; original required capabilities: {original}; previous attempted capabilities: {previous}; suggest only a strict subset already present in original required capabilities"
    )
}

fn candidates_from_hits(
    original_required: &[Capability],
    hits: &[crate::aeon::MemoryHit],
) -> Vec<Capability> {
    let content = hits
        .iter()
        .map(|hit| hit.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    original_required
        .iter()
        .filter(|capability| capability_matches(&content, capability))
        .cloned()
        .collect()
}

fn capability_matches(content: &str, capability: &Capability) -> bool {
    if content.is_empty() {
        return false;
    }

    let description = capability.description().to_ascii_lowercase();
    let debug_name = format!("{capability:?}").to_ascii_lowercase();
    content.contains(&description)
        || content.contains(&debug_name)
        || capability_aliases(capability)
            .iter()
            .any(|alias| content.contains(alias))
}

fn capability_aliases(capability: &Capability) -> Vec<String> {
    match capability {
        Capability::ReadFile(path) => vec![
            "readfile".to_string(),
            format!("read file {}", path.display()).to_ascii_lowercase(),
        ],
        Capability::WriteFile(path) => vec![
            "writefile".to_string(),
            format!("write file {}", path.display()).to_ascii_lowercase(),
        ],
        Capability::ListDirectory(path) => vec![
            "listdirectory".to_string(),
            format!("list directory {}", path.display()).to_ascii_lowercase(),
        ],
        Capability::HttpGet(url) => vec![
            "httpget".to_string(),
            format!("http get {url}").to_ascii_lowercase(),
        ],
        Capability::HttpPost(url) => vec![
            "httppost".to_string(),
            format!("http post {url}").to_ascii_lowercase(),
        ],
        Capability::ExecuteBinary(path) => vec![
            "executebinary".to_string(),
            format!("execute binary {}", path.display()).to_ascii_lowercase(),
        ],
        Capability::MountTmpfs(path) => vec![
            "mounttmpfs".to_string(),
            format!("mount tmpfs {}", path.display()).to_ascii_lowercase(),
        ],
        Capability::All => vec!["all".to_string()],
        Capability::None => vec!["none".to_string()],
    }
}

fn strict_intersection(
    original_required: &[Capability],
    candidates: &[Capability],
) -> Vec<Capability> {
    let mut narrowed = Vec::new();

    for capability in original_required {
        if candidates.contains(capability) && !narrowed.contains(capability) {
            narrowed.push(capability.clone());
        }
    }

    narrowed.retain(|capability| original_required.contains(capability));
    if narrowed.len() >= original_required.len() {
        Vec::new()
    } else {
        narrowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aeon::{AeonConfig, TestHttpResponse};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    type CapturedRequests = Arc<Mutex<Vec<crate::aeon::TestHttpRequest>>>;

    fn test_config(management_key: Option<&str>) -> AeonConfig {
        AeonConfig {
            enabled: true,
            base_url: "http://aeon.test".to_string(),
            agent_id: "agent-1".to_string(),
            session_id: None,
            timeout_ms: 100,
            management_key: management_key.map(str::to_string),
            hmac_key: None,
        }
    }

    fn memory_client(content: &'static str) -> (AeonMemoryClient, CapturedRequests) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_responder = Arc::clone(&captured);
        let client = AeonMemoryClient::with_test_responder(
            &test_config(Some("mgmt-key")),
            Arc::new(move |request| {
                captured_for_responder.lock().unwrap().push(request);
                TestHttpResponse {
                    status: 200,
                    body: format!(
                        r#"{{"results":[{{"id":"mem-1","content":{content:?},"score":0.9}}]}}"#
                    ),
                }
            }),
        );
        (client, captured)
    }

    fn unconfigured_memory_client() -> AeonMemoryClient {
        AeonMemoryClient::from_config(&test_config(None))
    }

    fn read_capability(path: &str) -> Capability {
        Capability::ReadFile(PathBuf::from(path))
    }

    fn write_capability(path: &str) -> Capability {
        Capability::WriteFile(PathBuf::from(path))
    }

    #[tokio::test]
    async fn two_round_cap_is_hard() {
        let manager = CapabilityManager::new();
        let (memory, captured) = memory_client("prefer read:/allowed");
        let original_required = vec![read_capability("/allowed"), write_capability("/blocked")];

        let outcome = negotiate_capability_denial(&original_required, &[], &manager, &memory).await;

        assert_eq!(outcome, None);
        assert!(captured.lock().unwrap().len() <= MAX_NEGOTIATION_ROUNDS);
    }

    #[tokio::test]
    async fn no_escalation_memory_suggestion_outside_original_is_rejected() {
        let (memory, _) = memory_client("try write:/outside instead");
        let original_required = vec![read_capability("/allowed")];
        let authorized_attempts = Arc::new(Mutex::new(Vec::<Vec<Capability>>::new()));
        let authorized_attempts_for_closure = Arc::clone(&authorized_attempts);

        let outcome =
            negotiate_capability_denial_with_authorizer(&original_required, &memory, |narrowed| {
                authorized_attempts_for_closure
                    .lock()
                    .unwrap()
                    .push(narrowed.to_vec());
                true
            })
            .await;

        assert_eq!(outcome, None);
        assert!(authorized_attempts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn subset_narrowing_succeeds() {
        let mut manager = CapabilityManager::new();
        let allowed = read_capability("/allowed");
        let blocked = write_capability("/blocked");
        let token = manager
            .issue(allowed.clone(), "test", Duration::from_secs(60))
            .unwrap();
        let (memory, _) = memory_client("use read:/allowed only");
        let original_required = vec![allowed.clone(), blocked];

        let outcome =
            negotiate_capability_denial(&original_required, &[token], &manager, &memory).await;

        assert_eq!(
            outcome,
            Some(NegotiationOutcome {
                narrowed_capabilities: vec![allowed],
                rounds: 1,
                evidence_hits: vec![crate::aeon::MemoryHit {
                    id: "mem-1".to_string(),
                    content: "use read:/allowed only".to_string(),
                    score: Some(0.9),
                }],
            })
        );
    }

    #[tokio::test]
    async fn fail_open_when_memory_unavailable() {
        let manager = CapabilityManager::new();
        let memory = unconfigured_memory_client();
        let original_required = vec![read_capability("/allowed"), write_capability("/blocked")];

        let outcome = negotiate_capability_denial(&original_required, &[], &manager, &memory).await;

        assert_eq!(outcome, None);
    }
}
