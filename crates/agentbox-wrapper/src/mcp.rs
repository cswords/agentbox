use std::future::Future;
use std::borrow::Cow;
use std::sync::Arc;

use axum::Router;
use rmcp::{
    handler::server::{
        tool::schema_for_type,
        wrapper::Parameters,
        ServerHandler,
    },
    model::{
        CallToolRequestParams, CallToolResult, Content, ListToolsResult,
        PaginatedRequestParams, Tool,
    },
    schemars,
    service::{MaybeSendFuture, RequestContext, RoleServer},
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::drivers::AgentDriver;

// --- MCP Tool Parameters ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunAgentRequest {
    #[schemars(description = "The task description or question for the agent. Be specific and actionable.")]
    pub prompt: String,
    #[schemars(
        description = "Optional: reuse the same value across calls to maintain conversation context. Omit for single-turn interactions."
    )]
    pub session_id: Option<String>,
}

// --- MCP Server Handler ---

#[derive(Clone)]
pub struct McpServer {
    driver: Arc<dyn AgentDriver>,
    model: String,
}

impl McpServer {
    pub fn new(driver: Arc<dyn AgentDriver>, model: String) -> Self {
        Self { driver, model }
    }

    /// Build a runtime `Tool` descriptor whose description reflects the
    /// active model, so LLMs reading `tools/list` can see which model
    /// powers this container.
    fn make_tool(&self) -> Tool {
        let description = format!(
            "Run a prompt through a containerized AI agent powered by {}.\n\n\
             This agent runs inside a Docker container with full filesystem \
             access to its workspace. It can read, write, and edit files, \
             execute shell commands, and invoke any MCP tools that have been \
             configured.\n\n\
             Parameters:\n  \
             - prompt (string, required): The task description or question. \
             Be specific about what you want — the agent works best with \
             clear, actionable instructions.\n  \
             - session_id (string, optional): Reuse the same value across \
             multiple calls to maintain conversation context. The agent will \
             remember previous turns in the same session. Omit this for \
             single-turn (stateless) interactions.",
            self.model,
        );
        Tool::new_with_raw(
            "run_agent",
            Some(Cow::Owned(description)),
            schema_for_type::<Parameters<RunAgentRequest>>(),
        )
        .with_title(format!("AgentBox — {}", self.model))
    }
}

// --- Manual ServerHandler (replaces #[tool_router(server_handler)]) ---
//
// We implement ServerHandler by hand so that `description` and `title`
// in the tool metadata can include the runtime model name.  The
// `#[tool]` / `#[tool_router]` macros bake descriptions into static
// strings at compile time — here we construct them at runtime.

impl ServerHandler for McpServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let req: RunAgentRequest =
            serde_json::from_value(serde_json::Value::Object(args)).map_err(|e| {
                ErrorData::invalid_params(format!("Failed to parse arguments: {e}"), None)
            })?;

        match self
            .driver
            .run_prompt(&req.prompt, req.session_id.as_deref())
            .await
        {
            Ok(result) => {
                let output = if let Some(sid) = &result.session_id {
                    format!("{}\n\n[session_id: {}]", result.output, sid)
                } else {
                    result.output
                };
                Ok(CallToolResult::success(vec![Content::text(output)]))
            }
            Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        let tool = self.make_tool();
        std::future::ready(Ok(ListToolsResult {
            tools: vec![tool],
            next_cursor: None,
            meta: None,
        }))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if name == "run_agent" {
            Some(self.make_tool())
        } else {
            None
        }
    }
}

/// Build the MCP router, mounted at /mcp.
pub fn mcp_router(
    driver: Arc<dyn AgentDriver>,
    ct: CancellationToken,
    model: String,
) -> anyhow::Result<Router> {
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(driver.clone(), model.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(["*"])
            .with_cancellation_token(ct),
    );

    Ok(Router::new().nest_service("/mcp", service))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::mock::MockDriver;

    fn test_model() -> String {
        "test-model".into()
    }

    #[tokio::test]
    async fn mcp_router_builds_without_panic() {
        let driver: Arc<dyn AgentDriver> = Arc::new(MockDriver::success("ok"));
        let ct = CancellationToken::new();
        let router = mcp_router(driver, ct, test_model());
        assert!(router.is_ok());
    }

    #[tokio::test]
    async fn run_agent_success_returns_output() {
        let driver: Arc<dyn AgentDriver> =
            Arc::new(MockDriver::success("refactored code here"));
        let server = McpServer::new(driver, test_model());

        // Call the underlying driver directly to verify the logic path
        let result = server.driver.run_prompt("test prompt", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().output, "refactored code here");
    }

    #[tokio::test]
    async fn run_agent_failure_returns_error_string() {
        let driver: Arc<dyn AgentDriver> = Arc::new(MockDriver::failure());
        let server = McpServer::new(driver, test_model());

        let result = server.driver.run_prompt("test prompt", None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mock driver failure"));
    }

    #[tokio::test]
    async fn make_tool_reflects_model_in_description() {
        let driver: Arc<dyn AgentDriver> = Arc::new(MockDriver::success("ok"));
        let server = McpServer::new(driver, "Gemini 3.1 Pro (High)".into());
        let tool = server.make_tool();
        assert_eq!(tool.name, "run_agent");
        assert!(tool.title.unwrap().contains("Gemini 3.1 Pro (High)"));
        let desc = tool.description.unwrap();
        assert!(desc.contains("Gemini 3.1 Pro (High)"), "description should contain model name: {desc}");
        assert!(desc.contains("prompt (string, required)"), "description should document parameters: {desc}");
    }
}
