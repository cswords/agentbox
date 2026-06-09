use std::time::Instant;

use anyhow::{Context, Result};
use tokio::process::Command;

use super::{AgentDriver, RunResult};
use crate::output_parser;

/// Driver for GitHub Copilot CLI.
///
/// Single-turn mode: spawns `copilot -p "<prompt>" --model <model>` and
/// captures stdout.  Does not support multi-turn sessions because Copilot
/// CLI's programmatic `-p` flag runs one-shot and exits.
pub struct CopilotDriver {
    pub workspace: String,
    pub model: String,
    pub yolo: bool,
}

impl std::fmt::Debug for CopilotDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopilotDriver")
            .field("workspace", &self.workspace)
            .field("model", &self.model)
            .field("yolo", &self.yolo)
            .finish()
    }
}

impl CopilotDriver {
    pub fn new(workspace: String, yolo: bool, model: String) -> Self {
        Self {
            workspace,
            yolo,
            model,
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for CopilotDriver {
    /// Copilot CLI `-p` mode outputs plain text without TUI chrome.
    /// We only need to strip ANSI escape sequences.
    fn clean_output(&self, raw: &str) -> String {
        output_parser::extract_response(raw)
    }

    async fn run_prompt(&self, prompt: &str, session_id: Option<&str>) -> Result<RunResult> {
        if session_id.is_some() {
            anyhow::bail!("Copilot driver does not support multi-turn sessions (use session_mode=single)");
        }

        let start = Instant::now();

        let mut cmd = Command::new("copilot");
        cmd.arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            .current_dir(&self.workspace);

        if self.yolo {
            cmd.arg("--allow-all-tools");
        }

        let output = cmd
            .output()
            .await
            .context("Failed to spawn copilot — is it installed (npm i -g @github/copilot) and in PATH?")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let duration_ms = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            tracing::warn!(
                exit_code = ?output.status.code(),
                stderr = %stderr,
                "copilot exited with error"
            );
        }

        let raw = if stdout.is_empty() { stderr } else { stdout };
        let cleaned = self.clean_output(&raw);

        Ok(RunResult {
            output: cleaned,
            session_id: None,
            duration_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_driver() -> CopilotDriver {
        CopilotDriver::new("/tmp".into(), true, "claude-opus-4.6".into())
    }

    #[test]
    fn driver_new_stores_fields() {
        let d = CopilotDriver::new("/workspace".into(), false, "gpt-5.5".into());
        assert_eq!(d.workspace, "/workspace");
        assert!(!d.yolo);
        assert_eq!(d.model, "gpt-5.5");
    }

    #[test]
    fn debug_format_excludes_sensitive() {
        let d = make_driver();
        let s = format!("{d:?}");
        assert!(s.contains("CopilotDriver"));
        assert!(s.contains("claude-opus-4.6"));
        assert!(!s.contains("token"));
    }

    #[test]
    fn clean_output_passes_plain_text() {
        let d = make_driver();
        let result = d.clean_output("Hello from Copilot");
        assert_eq!(result, "Hello from Copilot");
    }

    #[test]
    fn clean_output_strips_ansi() {
        let d = make_driver();
        let raw = "\u{1b}[32mGreen text\u{1b}[0m";
        let result = d.clean_output(raw);
        assert!(!result.contains('\u{1b}'));
        assert!(result.contains("Green text"));
    }
}
