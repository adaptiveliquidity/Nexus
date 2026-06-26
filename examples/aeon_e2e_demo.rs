#![cfg(feature = "aeon-memory")]

use std::env;
use std::time::Duration;

use anyhow::Result;
use nexus::daemon::NexusExecutionEvent;
use nexus::proof::schema::MemoryAttestationMode;
use nexus::{
    AeonConfig, Capability, HypervisorConfig, NexusHypervisor, ToolDefinition, ToolOutput,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let (base_url, mock_server) = start_mock_aeon().await?;
    let agent_id = env::var("NEXUS_AEON_AGENT_ID").unwrap_or_else(|_| "demo-agent".to_string());
    let session_id =
        env::var("NEXUS_AEON_SESSION_ID").unwrap_or_else(|_| "demo-session".to_string());
    let management_key = env::var("NEXUS_AEON_MANAGEMENT_KEY")
        .unwrap_or_else(|_| format!("mock-{}", Uuid::new_v4()));
    let hmac_key =
        env_hex_key("NEXUS_AEON_HMAC_KEY")?.unwrap_or_else(|| Uuid::new_v4().as_bytes().to_vec());

    let aeon = AeonConfig {
        enabled: true,
        base_url,
        agent_id: agent_id.clone(),
        session_id: Some(session_id.clone()),
        timeout_ms: 500,
        management_key: Some(management_key),
        hmac_key: Some(hmac_key),
    };
    let hypervisor = NexusHypervisor::new(HypervisorConfig {
        aeon_config: Some(aeon),
        ..HypervisorConfig::default()
    })?;

    let allowed = Capability::ReadFile("/allowed".into());
    let blocked = Capability::WriteFile("/blocked".into());
    let caller_token =
        hypervisor.issue_token(allowed.clone(), "aeon_e2e_demo", Duration::from_secs(60))?;

    let tool = ToolDefinition::new("aeon_e2e_noop".to_string(), noop_wasm())
        .with_capabilities(vec![allowed, blocked])
        .with_aeon_context(Some(agent_id.clone()), Some(session_id.clone()));

    let (output, capsule) = hypervisor
        .execute_tool_proof_with_tokens(tool, serde_json::json!({"demo": true}), &[caller_token])
        .await?;

    assert!(output.success);
    assert_eq!(capsule.capabilities.negotiation_rounds, Some(1));
    assert_eq!(
        capsule.memory_mode,
        Some(MemoryAttestationMode::AttestedWithRecall)
    );
    assert!(capsule.memory_evidence.is_some());

    let events = timeline_events(
        &output,
        capsule.capsule_id,
        capsule.capabilities.negotiation_rounds,
    );
    println!("proof_capsule_id={}", capsule.capsule_id);
    println!("memory_mode={:?}", capsule.memory_mode);
    println!(
        "memory_evidence_injected_count={}",
        capsule
            .memory_evidence
            .as_ref()
            .map(|evidence| evidence.injected_count)
            .unwrap_or_default()
    );
    println!(
        "negotiation_rounds={:?}",
        capsule.capabilities.negotiation_rounds
    );
    println!("forward_events_to=/agents/{agent_id}/timeline");
    println!("{}", serde_json::to_string_pretty(&events)?);

    mock_server.abort();
    Ok(())
}

fn noop_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#).unwrap()
}

fn timeline_events(
    output: &ToolOutput,
    capsule_id: Uuid,
    negotiation_rounds: Option<u32>,
) -> Vec<NexusExecutionEvent> {
    let mut events = Vec::new();
    if negotiation_rounds.is_some() {
        events.push(NexusExecutionEvent::CapabilityDenied {
            denied_category: "capability_denial_negotiated".to_string(),
        });
    }
    if let Some(snapshot_id) = output.snapshot_id {
        events.push(NexusExecutionEvent::SnapshotCreated { snapshot_id });
    }
    events.push(NexusExecutionEvent::ProofCapsuleEmitted { capsule_id });
    events
}

async fn start_mock_aeon() -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0_u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }

                let request = String::from_utf8_lossy(&buf[..n]);
                let body = if request.starts_with("POST /api/v1/memories/search ") {
                    serde_json::json!({
                        "results": [{
                            "id": "mem-allowed-read",
                            "content": "use read:/allowed only",
                            "score": 0.91
                        }]
                    })
                    .to_string()
                } else {
                    serde_json::json!({
                        "id": Uuid::new_v4()
                    })
                    .to_string()
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });

    Ok((format!("http://{addr}"), handle))
}

fn env_hex_key(name: &str) -> Result<Option<Vec<u8>>> {
    let Ok(raw) = env::var(name) else {
        return Ok(None);
    };
    if raw.len() % 2 != 0 {
        anyhow::bail!("{name} must contain an even number of hex characters");
    }

    let bytes = (0..raw.len())
        .step_by(2)
        .map(|idx| u8::from_str_radix(&raw[idx..idx + 2], 16))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(bytes))
}
