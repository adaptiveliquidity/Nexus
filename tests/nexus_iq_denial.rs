#![cfg(feature = "aeon-memory")]

mod nexus_iq_support;

use nexus_iq_support::{initialize_client, iq_args_stub, tool_json, McpClient, MockAeonServer};
use serde_json::{json, Value};

fn capability_allowlist(path: &str) -> String {
    json!([{ "type": "read_file", "path": path }]).to_string()
}

async fn call_nexus_iq_execute(client: &mut McpClient, args: Value) -> Value {
    let resp = client
        .request(
            2,
            "tools/call",
            json!({ "name": "nexus_iq_execute", "arguments": args }),
        )
        .await;
    tool_json(&resp)
}

fn assert_denied(parsed: &Value) {
    assert_eq!(parsed["denied"], true, "expected denial: {parsed}");
    assert_eq!(parsed["output"], Value::Null);
    assert_eq!(parsed["proof_capsule_ref"], Value::Null);
    assert_eq!(parsed["denial_negotiation"]["denied"], true);
    assert_eq!(parsed["denial_negotiation"]["negotiated"], false);
    assert!(
        parsed["denial_negotiation"]["reason"].is_string(),
        "denial should include a reason: {parsed}"
    );
}

#[tokio::test]
async fn tool_not_in_allowlist() {
    let mut client = McpClient::spawn_with_extra_env(
        None,
        None,
        [("NEXUS_IQ_ALLOWLIST", json!(["some_other_tool"]).to_string())],
    )
    .await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["tool_name"] = json!("blocked_tool");

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
    assert!(
        parsed["denial_negotiation"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("NEXUS_IQ_ALLOWLIST"),
        "denial should identify the IQ allowlist: {parsed}"
    );
}

#[tokio::test]
async fn capability_denial_no_token() {
    let mut client = McpClient::spawn_without_aeon(None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["required_capabilities"] = json!(["read:/etc"]);

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
}

#[tokio::test]
async fn capability_denial_wrong_scope() {
    let mut client = McpClient::spawn_without_aeon(Some(capability_allowlist("/tmp"))).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["required_capabilities"] = json!(["read:/etc"]);

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
}

#[tokio::test]
async fn negotiation_rounds_populated() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"use read:/allowed only","score":0.9}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn(&server, Some(capability_allowlist("/allowed"))).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["required_capabilities"] = json!(["read:/allowed", "write:/blocked"]);

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_eq!(
        parsed["denied"], false,
        "negotiation should permit run: {parsed}"
    );
    assert_eq!(parsed["denial_negotiation"]["denied"], false);
    assert_eq!(parsed["denial_negotiation"]["negotiated"], true);
    assert!(
        parsed["denial_negotiation"]["rounds"].as_u64().is_some(),
        "negotiation should report rounds: {parsed}"
    );
}

#[tokio::test]
async fn denial_still_returns_memory_evidence() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"previous context","score":0.91}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn(&server, None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["memory_query"] = json!("recall context before denial");
    args["memory_limit"] = json!(5);
    args["required_capabilities"] = json!(["read:/denied"]);

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
    assert_eq!(parsed["memory_hits_count"], 1);
    assert_eq!(
        parsed["memory_evidence_ref"]["attestation"],
        "AttestedWithRecall"
    );
    assert_eq!(
        parsed["memory_evidence_ref"]["query"],
        "recall context before denial"
    );
    assert!(
        parsed["memory_evidence_ref"]["hit_digests"]
            .as_array()
            .is_some_and(|digests| !digests.is_empty()),
        "denial should preserve memory evidence: {parsed}"
    );
}

#[tokio::test]
async fn denial_still_posts_timeline() {
    let Some(server) = MockAeonServer::try_new(200, r#"{"results":[]}"#, 200) else {
        return;
    };
    let mut client = McpClient::spawn(&server, None).await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["required_capabilities"] = json!(["read:/denied"]);

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
    assert_eq!(parsed["timeline_status"], "fire_and_forget");
    server
        .wait_for_path("/api/v1/agents/agent-1/timeline", 1)
        .await;
    let captured = server.captured_requests();
    let timeline = captured
        .iter()
        .find(|request| request.path == "/api/v1/agents/agent-1/timeline")
        .expect("timeline request should be captured");
    let body: Value = serde_json::from_str(&timeline.body).expect("timeline body should be JSON");
    assert_eq!(body["event_type"], "capability_denied");
    assert_eq!(body["session_id"], "session-1");
}

#[tokio::test]
async fn allowlist_denial_skips_memory_recall() {
    let Some(server) = MockAeonServer::try_new(
        200,
        r#"{"results":[{"id":"mem-1","content":"some context","score":0.95}]}"#,
        200,
    ) else {
        return;
    };
    let mut client = McpClient::spawn_with_extra_env(
        Some(server.base_url()),
        None,
        [("NEXUS_IQ_ALLOWLIST", json!(["some_other_tool"]).to_string())],
    )
    .await;
    initialize_client(&mut client).await;

    let mut args = iq_args_stub();
    args["tool_name"] = json!("blocked_tool");
    args["memory_query"] = json!("recall memory query");

    let parsed = call_nexus_iq_execute(&mut client, args).await;

    assert_denied(&parsed);
    assert_eq!(parsed["memory_hits_count"], 0);
    assert_eq!(parsed["memory_evidence_ref"]["attestation"], "Absent");

    let captured = server.captured_requests();
    let search_req = captured
        .iter()
        .find(|request| request.path.contains("search"));
    assert!(
        search_req.is_none(),
        "Expected no memory search requests to be made, but found: {:?}",
        search_req
    );
}
