pub mod antigravity;
pub mod copilot;

use std::sync::Arc;

use anyhow::Result;

use crate::config::AppConfig;

/// Result of running a prompt through an agent.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RunResult {
    pub output: String,
    pub session_id: Option<String>,
    pub duration_ms: u64,
}

/// Common interface for all agent drivers.
#[async_trait::async_trait]
pub trait AgentDriver: Send + Sync {
    async fn run_prompt(&self, prompt: &str, session_id: Option<&str>) -> Result<RunResult>;

    /// Close a multi-turn session. Default is a no-op for drivers that
    /// don't support sessions.
    async fn close_session(&self, _session_id: &str) {}

    /// Perform driver-specific initialization (write config files, first-run
    /// setup, health check, etc.). Called once at startup. Default: no-op.
    async fn initialize(&self) -> Result<()> {
        Ok(())
    }

    /// Clean driver-specific TUI decorations from raw agent output.
    /// Default: strips ANSI escape codes and generic TUI chrome.
    fn clean_output(&self, raw: &str) -> String {
        crate::output_parser::extract_response(raw)
    }
}

/// Create the appropriate driver for the configured agent type.
/// Calls driver.initialize() before returning.
pub async fn create_driver(config: &AppConfig) -> Result<Arc<dyn AgentDriver>> {
    let driver: Arc<dyn AgentDriver> = match config.agent.as_str() {
        "antigravity" => Arc::new(antigravity::AntigravityDriver::new(
            config.workspace.clone(),
            config.yolo,
            config.model.clone(),
            config.mcp_servers.clone(),
        )),
        "copilot" => Arc::new(copilot::CopilotDriver::new(
            config.workspace.clone(),
            config.yolo,
            config.model.clone(),
        )),
        other => anyhow::bail!("Unknown agent type: {other}. Supported: antigravity, copilot"),
    };
    driver.initialize().await?;
    Ok(driver)
}

/// Mock driver for unit tests.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    pub struct MockDriver {
        pub response: String,
        pub should_fail: bool,
        pub last_prompt: Mutex<Option<String>>,
        pub last_session_id: Mutex<Option<String>>,
    }

    impl MockDriver {
        pub fn success(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
                should_fail: false,
                last_prompt: Mutex::new(None),
                last_session_id: Mutex::new(None),
            }
        }

        pub fn failure() -> Self {
            Self {
                response: String::new(),
                should_fail: true,
                last_prompt: Mutex::new(None),
                last_session_id: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentDriver for MockDriver {
        async fn run_prompt(
            &self,
            prompt: &str,
            session_id: Option<&str>,
        ) -> Result<RunResult> {
            *self.last_prompt.lock().unwrap() = Some(prompt.to_owned());
            *self.last_session_id.lock().unwrap() = session_id.map(|s| s.to_owned());
            if self.should_fail {
                Err(anyhow::anyhow!("mock driver failure"))
            } else {
                Ok(RunResult {
                    output: self.response.clone(),
                    // Echo back the session_id if provided
                    session_id: session_id.map(|s| s.to_owned()),
                    duration_ms: 0,
                })
            }
        }

        async fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn clean_output(&self, raw: &str) -> String {
            raw.to_string()
        }
    }
}
