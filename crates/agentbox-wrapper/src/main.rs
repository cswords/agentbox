mod acp;
mod config;
mod drivers;
mod mcp;
mod output_parser;
mod session;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::AppConfig;
use crate::drivers::AgentDriver;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration from environment
    let config = AppConfig::from_env()?;
    tracing::info!(
        model = %config.model,
        agent = %config.agent,
        port = config.port,
        workspace = %config.workspace,
        session_mode = ?config.session_mode,
        "agentbox-wrapper starting"
    );

    // Create the agent driver via factory (handles initialization)
    let driver: Arc<dyn AgentDriver> = drivers::create_driver(&config).await?;

    // Shared state for ACP endpoints
    let acp_state = Arc::new(acp::AppState {
        config: config.clone(),
        driver: driver.clone(),
        runs: RwLock::new(HashMap::new()),
    });

    // Cancellation token for graceful shutdown
    let ct = CancellationToken::new();

    // Build combined router: ACP + MCP + health
    let app = Router::new()
        .merge(acp::acp_router(acp_state))
        .merge(mcp::mcp_router(driver, ct.child_token(), config.model.clone())?)
        .route("/health", get(|| async { "ok" }));

    // Start server
    let bind_addr = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!(addr = %bind_addr, "Listening — MCP at /mcp, ACP at /agents & /runs");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Shutting down");
            ct.cancel();
        })
        .await?;

    Ok(())
}
