use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_stream::wrappers::ReceiverStream;

use crate::config::AppConfig;
use crate::drivers::AgentDriver;

/// Shared application state for ACP endpoints.
pub struct AppState {
    pub config: AppConfig,
    pub driver: Arc<dyn AgentDriver>,
    pub runs: RwLock<std::collections::HashMap<String, RunRecord>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub output: Option<Vec<MessagePart>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    pub role: String,
    pub parts: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

// --- Request / Response types ---

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct CreateRunRequest {
    pub agent_name: String,
    pub input: Vec<MessagePart>,
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateRunResponse {
    pub run_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub output: Vec<MessagePart>,
}

#[derive(Debug, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ListAgentsResponse {
    pub agents: Vec<AgentInfo>,
}

// --- Core run logic (shared between JSON and SSE handlers) ---

/// Execute an agent run and return the result. Shared between sync and SSE paths.
async fn execute_run(
    state: &AppState,
    req: &CreateRunRequest,
) -> Result<(String, Option<String>, Vec<MessagePart>), StatusCode> {
    let prompt: String = req
        .input
        .iter()
        .flat_map(|m| m.parts.iter())
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    if prompt.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let run_id = format!("run_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));

    {
        let mut runs = state.runs.write().await;
        runs.insert(
            run_id.clone(),
            RunRecord {
                run_id: run_id.clone(),
                session_id: req.session_id.clone(),
                status: "running".into(),
                output: None,
            },
        );
    }

    let result = state
        .driver
        .run_prompt(&prompt, req.session_id.as_deref())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Agent run failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let output = vec![MessagePart {
        role: "agent".into(),
        parts: vec![ContentPart {
            content_type: "text".into(),
            text: result.output,
        }],
    }];

    let response_session_id = result.session_id.or_else(|| req.session_id.clone());

    {
        let mut runs = state.runs.write().await;
        if let Some(record) = runs.get_mut(&run_id) {
            record.status = "completed".into();
            record.session_id = response_session_id.clone();
            record.output = Some(output.clone());
        }
    }

    Ok((run_id, response_session_id, output))
}

// --- Handlers ---

async fn list_agents(State(state): State<Arc<AppState>>) -> Json<ListAgentsResponse> {
    Json(ListAgentsResponse {
        agents: vec![AgentInfo {
            name: state.config.agent.clone(),
            description: format!(
                "{} CLI agent ({})",
                state.config.agent, state.config.model
            ),
            metadata: serde_json::json!({
                "model": state.config.model,
                "session_mode": match state.config.session_mode {
                    crate::config::SessionMode::Single => "single",
                    crate::config::SessionMode::Multi => "multi",
                }
            }),
        }],
    })
}

async fn create_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateRunRequest>,
) -> Response {
    let wants_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));

    if wants_sse {
        return create_run_sse(state, req).await;
    }

    // Sync JSON path
    match execute_run(&state, &req).await {
        Ok((run_id, session_id, output)) => Json(CreateRunResponse {
            run_id,
            session_id,
            status: "completed".into(),
            output,
        })
        .into_response(),
        Err(status) => status.into_response(),
    }
}

/// SSE streaming handler — emits started → output → completed events.
async fn create_run_sse(state: Arc<AppState>, req: CreateRunRequest) -> Response {
    let run_id = format!("run_{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    let session_id = req.session_id.clone();

    // Store initial "running" record
    {
        let mut runs = state.runs.write().await;
        runs.insert(
            run_id.clone(),
            RunRecord {
                run_id: run_id.clone(),
                session_id: session_id.clone(),
                status: "running".into(),
                output: None,
            },
        );
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(8);

    let sse_run_id = run_id.clone();
    let sse_session_id = session_id.clone();

    tokio::spawn(async move {
        // Event 1: started
        let _ = tx
            .send(Ok(Event::default()
                .event("started")
                .data(serde_json::json!({
                    "run_id": sse_run_id,
                    "status": "running"
                }).to_string())))
            .await;

        // Extract prompt
        let prompt: String = req.input.iter()
            .flat_map(|m| m.parts.iter())
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        if prompt.is_empty() {
            let _ = tx.send(Ok(Event::default()
                .event("error")
                .data(serde_json::json!({"error": "empty prompt"}).to_string())))
            .await;
            return;
        }

        // Run the agent
        let result = state.driver.run_prompt(&prompt, req.session_id.as_deref()).await;

        match result {
            Ok(r) => {
                let response_session_id = r.session_id.or(sse_session_id);
                let output_parts = vec![MessagePart {
                    role: "agent".into(),
                    parts: vec![ContentPart {
                        content_type: "text".into(),
                        text: r.output,
                    }],
                }];

                // Event 2: output
                let _ = tx.send(Ok(Event::default()
                    .event("output")
                    .data(serde_json::json!({
                        "parts": output_parts[0].parts.iter()
                            .map(|p| serde_json::json!({"type": p.content_type, "text": p.text}))
                            .collect::<Vec<_>>()
                    }).to_string())))
                .await;

                // Event 3: completed
                let _ = tx.send(Ok(Event::default()
                    .event("completed")
                    .data(serde_json::json!({
                        "run_id": sse_run_id,
                        "session_id": response_session_id,
                        "status": "completed"
                    }).to_string())))
                .await;

                // Update run record
                let mut runs = state.runs.write().await;
                if let Some(record) = runs.get_mut(&sse_run_id) {
                    record.status = "completed".into();
                    record.session_id = response_session_id;
                    record.output = Some(output_parts);
                }
            }
            Err(e) => {
                let _ = tx.send(Ok(Event::default()
                    .event("error")
                    .data(serde_json::json!({
                        "run_id": sse_run_id,
                        "error": e.to_string()
                    }).to_string())))
                .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn get_run(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<Json<RunRecord>, StatusCode> {
    let runs = state.runs.read().await;
    runs.get(&run_id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn delete_run(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
) -> Result<StatusCode, StatusCode> {
    let session_id = {
        let mut runs = state.runs.write().await;
        match runs.remove(&run_id) {
            Some(record) => record.session_id,
            None => return Err(StatusCode::NOT_FOUND),
        }
    };

    // If the run had a session, close it via the driver
    if let Some(sid) = session_id {
        state.driver.close_session(&sid).await;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Build the ACP router.
pub fn acp_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/agents", get(list_agents))
        .route("/runs", post(create_run))
        .route("/runs/{run_id}", get(get_run).delete(delete_run))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SessionMode;
    use crate::drivers::mock::MockDriver;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use tower::ServiceExt;

    fn test_config() -> AppConfig {
        AppConfig {
            model: "test-model".into(),
            agent: "antigravity".into(),
            port: 7080,
            session_mode: SessionMode::Single,
            yolo: false,
            workspace: "/workspace".into(),
            session_timeout_secs: 1800,
            mcp_servers: None,
        }
    }

    fn test_state(driver: Arc<dyn AgentDriver>) -> Arc<AppState> {
        Arc::new(AppState {
            config: test_config(),
            driver,
            runs: RwLock::new(HashMap::new()),
        })
    }

    async fn body_to_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // --- GET /agents ---

    #[tokio::test]
    async fn list_agents_returns_agent_info() {
        let driver = Arc::new(MockDriver::success("hello"));
        let state = test_state(driver);
        let app = acp_router(state);

        let req = Request::builder()
            .uri("/agents")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();

        assert_eq!(body["agents"][0]["name"], "antigravity");
        assert_eq!(body["agents"][0]["metadata"]["model"], "test-model");
        assert_eq!(body["agents"][0]["metadata"]["session_mode"], "single");
    }

    // --- POST /runs ---

    #[tokio::test]
    async fn create_run_returns_completed_output() {
        let driver = Arc::new(MockDriver::success("I refactored the auth module."));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": "refactor auth"}]
            }]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();

        assert_eq!(body["status"], "completed");
        assert!(body["run_id"].as_str().unwrap().starts_with("run_"));
        assert_eq!(
            body["output"][0]["parts"][0]["text"],
            "I refactored the auth module."
        );
    }

    #[tokio::test]
    async fn create_run_empty_prompt_returns_400() {
        let driver = Arc::new(MockDriver::success("anything"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": ""}]
            }]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_run_driver_failure_returns_500() {
        let driver = Arc::new(MockDriver::failure());
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": "do something"}]
            }]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn create_run_stores_record_for_get_run() {
        let driver = Arc::new(MockDriver::success("result text"));
        let state = test_state(driver.clone());
        let app = acp_router(state.clone());

        // Create a run
        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": "hello"}]
            }]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();
        let run_id = body["run_id"].as_str().unwrap().to_owned();

        // Now fetch it
        let app2 = acp_router(state.clone());
        let req2 = Request::builder()
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);

        let body2: serde_json::Value =
            serde_json::from_str(&body_to_string(resp2.into_body()).await).unwrap();
        assert_eq!(body2["status"], "completed");
        assert_eq!(body2["run_id"], run_id);
    }

    // --- GET /runs/:id ---

    #[tokio::test]
    async fn get_run_unknown_id_returns_404() {
        let driver = Arc::new(MockDriver::success("anything"));
        let state = test_state(driver);
        let app = acp_router(state);

        let req = Request::builder()
            .uri("/runs/run_nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- Multi-turn / session_id ---

    #[tokio::test]
    async fn create_run_with_session_id_returns_session_id() {
        let driver = Arc::new(MockDriver::success("multi-turn response"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": "follow up question"}]
            }],
            "session_id": "sess_abc123"
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();

        assert_eq!(body["status"], "completed");
        assert_eq!(body["session_id"], "sess_abc123");
        assert_eq!(
            body["output"][0]["parts"][0]["text"],
            "multi-turn response"
        );
    }

    #[tokio::test]
    async fn create_run_without_session_id_has_null_session_id() {
        let driver = Arc::new(MockDriver::success("single-turn response"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{
                "role": "user",
                "parts": [{"type": "text", "text": "hello"}]
            }]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();

        assert!(body["session_id"].is_null());
    }

    // --- DELETE /runs/:id ---

    #[tokio::test]
    async fn delete_run_existing_returns_204() {
        let driver = Arc::new(MockDriver::success("result"));
        let state = test_state(driver.clone());
        let app = acp_router(state.clone());

        // First create a run
        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{"role": "user", "parts": [{"type": "text", "text": "do it"}]}],
            "session_id": "sess_to_delete"
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();
        let run_id = body["run_id"].as_str().unwrap().to_owned();

        // Now delete it
        let app2 = acp_router(state.clone());
        let req2 = Request::builder()
            .method("DELETE")
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let app3 = acp_router(state.clone());
        let req3 = Request::builder()
            .uri(format!("/runs/{run_id}"))
            .body(Body::empty())
            .unwrap();

        let resp3 = app3.oneshot(req3).await.unwrap();
        assert_eq!(resp3.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_run_nonexistent_returns_404() {
        let driver = Arc::new(MockDriver::success("anything"));
        let state = test_state(driver);
        let app = acp_router(state);

        let req = Request::builder()
            .method("DELETE")
            .uri("/runs/run_nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // --- SSE streaming ---

    #[tokio::test]
    async fn create_run_sse_returns_event_stream() {
        let driver = Arc::new(MockDriver::success("streamed response"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{"role":"user","parts":[{"type":"text","text":"hello"}]}]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/event-stream"),
            "Expected SSE content-type, got: {content_type}"
        );

        // Read the SSE body and check events
        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("event: started"), "Missing started event: {body}");
        assert!(body.contains("event: output"), "Missing output event: {body}");
        assert!(body.contains("event: completed"), "Missing completed event: {body}");
        assert!(
            body.contains("streamed response"),
            "Missing response text: {body}"
        );
    }

    #[tokio::test]
    async fn create_run_sse_empty_prompt_sends_error() {
        let driver = Arc::new(MockDriver::success("anything"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{"role":"user","parts":[{"type":"text","text":""}]}]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK); // SSE always returns 200

        let body = body_to_string(resp.into_body()).await;
        assert!(body.contains("event: error"), "Expected error event: {body}");
    }

    #[tokio::test]
    async fn create_run_without_sse_header_returns_json() {
        let driver = Arc::new(MockDriver::success("json response"));
        let state = test_state(driver);
        let app = acp_router(state);

        let payload = serde_json::json!({
            "agent_name": "antigravity",
            "input": [{"role":"user","parts":[{"type":"text","text":"test"}]}]
        });

        let req = Request::builder()
            .method("POST")
            .uri("/runs")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("application/json"),
            "Expected JSON content-type, got: {content_type}"
        );

        let body: serde_json::Value =
            serde_json::from_str(&body_to_string(resp.into_body()).await).unwrap();
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["parts"][0]["text"], "json response");
    }
}
