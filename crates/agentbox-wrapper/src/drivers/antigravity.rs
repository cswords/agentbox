use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use dirs;
use tokio::process::Command;
use tokio::sync::Mutex;

use super::{AgentDriver, RunResult};
use crate::output_parser::{self, strip_ansi};
use crate::session::PtySession;

/// Driver for Google Antigravity CLI (agy).
///
/// Single-turn mode: spawns `agy -p "<prompt>"` and captures stdout.
/// Multi-turn mode: maintains PTY sessions for interactive conversations.
pub struct AntigravityDriver {
    pub workspace: String,
    pub yolo: bool,
    pub model: String,
    pub mcp_servers: Option<serde_json::Value>,
    /// Active PTY sessions keyed by session ID.
    pub sessions: Arc<Mutex<HashMap<String, PtySession>>>,
}

impl std::fmt::Debug for AntigravityDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityDriver")
            .field("workspace", &self.workspace)
            .field("yolo", &self.yolo)
            .finish_non_exhaustive()
    }
}

impl AntigravityDriver {
    pub fn new(workspace: String, yolo: bool, model: String, mcp_servers: Option<serde_json::Value>) -> Self {
        Self {
            workspace,
            yolo,
            model,
            mcp_servers,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Write settings.json to the agy config directory.
    fn write_agy_settings(settings: &serde_json::Value) -> Result<()> {
        let config_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/root"))
            .join(".gemini")
            .join("antigravity-cli");

        std::fs::create_dir_all(&config_dir)
            .context("Failed to create agy config directory")?;

        let settings_path = config_dir.join("settings.json");
        let content = serde_json::to_string_pretty(settings)?;
        std::fs::write(&settings_path, content)
            .context("Failed to write agy settings.json")?;

        Ok(())
    }

    /// Multi-turn: inject a prompt into a PTY session and wait for the response.
    async fn run_multi_turn(&self, prompt: &str, session_id: Option<&str>) -> Result<RunResult> {
        let start = Instant::now();
        let mut sessions = self.sessions.lock().await;

        // Determine session ID (reuse existing or create new)
        let sid = session_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Check if session exists and is alive; remove if dead
        let need_create = if let Some(session) = sessions.get(&sid) {
            if !session.is_alive() {
                tracing::info!(session_id = %sid, "Session dead, will recreate");
                true
            } else {
                false
            }
        } else {
            true
        };

        if need_create {
            sessions.remove(&sid); // drop dead session if any

            let workspace = self.workspace.clone();
            let yolo = self.yolo;
            let model_name = self.model.clone();
            let mcp_config = self.mcp_servers.clone().unwrap_or(serde_json::Value::Null);
            let new_sid = sid.clone();

            // PtySession::new is blocking (PTY syscalls), so run on blocking thread
            let session = tokio::task::spawn_blocking(move || {
                // Re-write settings.json before spawning agy (agy overwrites it on startup)
                Self::write_agy_settings(&serde_json::json!({
                    "model": model_name,
                    "general": { "yolo": yolo },
                    "mcpServers": mcp_config
                })).ok();

                let mut args: Vec<&str> = Vec::new();
                if yolo {
                    args.push("--dangerously-skip-permissions");
                }
                let s = PtySession::with_cwd("agy", &args, 120, 40, Some(&workspace))?;

                // Wait for agy to finish initializing (shows prompt)
                std::thread::sleep(std::time::Duration::from_secs(3));
                let _ = s.wait_for_stable_sync(
                    std::time::Duration::from_millis(500),
                    std::time::Duration::from_millis(100),
                );

                tracing::info!(session_id = %new_sid, "Created new PTY session");
                Ok::<PtySession, anyhow::Error>(s)
            })
            .await
            .context("Session creation task panicked")??;

            sessions.insert(sid.clone(), session);

            // Dismiss any initial dialogs (e.g., Terms of Service on first run).
            // Poll the screen until the dialog appears or we time out.
            {
                let session = sessions.get(&sid).unwrap();
                for _ in 0..30 {
                    let screen = session.screen_text();
                    if screen.contains("Terms of Service") {
                        tracing::info!(session_id = %sid, "Dismissing Terms of Service dialog");
                        // Navigate down past checkbox + links to buttons, then right to Done, then Enter.
                        // Arrow key escape sequences: Down=\x1b[B, Right=\x1b[C
                        for _ in 0..8 {
                            session.write_raw(b"\x1b[B")?;
                            std::thread::sleep(std::time::Duration::from_millis(30));
                        }
                        session.write_raw(b"\x1b[C")?;
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        session.write_raw(b"\r")?;
                        // Wait for dialog to close
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let _ = session.wait_for_stable_sync(
                            std::time::Duration::from_millis(500),
                            std::time::Duration::from_millis(100),
                        );
                        break;
                    }
                    if screen.contains("Type a message") || screen.contains("Ready") {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }

        // Inject prompt and capture response via screen diff.
        // Retry once if ToS dialog appears (first-run consent screen).
        let response = {
            let session = sessions
                .get(&sid)
                .context("Session disappeared unexpectedly")?;

            let mut screen_before = session.screen_text();
            session.inject_prompt(prompt)?;

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            let mut screen_after = session
                .wait_for_stable(
                    std::time::Duration::from_secs(10),
                    std::time::Duration::from_millis(500),
                )
                .await?;

            let mut diff = self.compute_response_diff(&screen_before, &screen_after);

            // If ToS dialog appeared, dismiss it and retry the prompt
            if diff.contains("Terms of Service") {
                tracing::info!(session_id = %sid, "ToS dialog detected in response, dismissing...");
                // Navigate down past checkbox + links to Done button, then Enter.
                // Arrow key escape sequences: Down=\x1b[B, Right=\x1b[C
                for _ in 0..8 {
                    session.write_raw(b"\x1b[B")?;
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                session.write_raw(b"\x1b[C")?;
                std::thread::sleep(std::time::Duration::from_millis(50));
                session.write_raw(b"\r")?;
                // Wait for dialog to close
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let _ = session
                    .wait_for_stable(
                        std::time::Duration::from_secs(2),
                        std::time::Duration::from_millis(200),
                    )
                    .await;

                // Retry the prompt
                screen_before = session.screen_text();
                session.inject_prompt(prompt)?;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                screen_after = session
                    .wait_for_stable(
                        std::time::Duration::from_secs(30),
                        std::time::Duration::from_millis(500),
                    )
                    .await?;
                diff = self.compute_response_diff(&screen_before, &screen_after);
            }

            // If response still looks like a subagent is running, extend the wait
            // Subagents run asynchronously so the main screen may be stable while the
            // subagent works. We loop-wait until the subagent status disappears.
            if (diff.is_empty() || diff.contains("subagent"))
                && session.screen_text().contains("subagent(s)")
                && !session.screen_text().contains("0 subagent(s)")
            {
                let screen_snippet: String = session.screen_text().chars().take(200).collect();
                tracing::info!(session_id = %sid, screen = %screen_snippet, "Subagent detected, waiting for completion...");
                let screen_before2 = session.screen_text();
                // Poll in 30s chunks, up to 5 minutes total
                let mut total_wait = 0u64;
                let max_total_wait = 300u64; // 5 minutes
                loop {
                    let _screen_after2 = session
                        .wait_for_stable(
                            std::time::Duration::from_secs(30),
                            std::time::Duration::from_millis(500),
                        )
                        .await?;
                    let _current = session.screen_text();
                    // Check if subagent has finished (status bar gone)
                    // Check if diff has changed (result appeared on screen)
                    let current_screen = session.screen_text();
                    let new_diff = self.compute_response_diff(&screen_before2, &current_screen);
                    if new_diff != diff && !new_diff.trim().is_empty() {
                        diff = new_diff;
                        tracing::info!(session_id = %sid, total_wait_s = total_wait, "Subagent result captured from diff");
                        break;
                    }
                    total_wait += 30;
                    if total_wait >= max_total_wait {
                        tracing::warn!(session_id = %sid, total_wait_s = total_wait, "Subagent wait timeout");
                        diff = self.compute_response_diff(&screen_before2, &current_screen);
                        break;
                    }
                    tracing::info!(session_id = %sid, total_wait_s = total_wait, "Subagent still running, waiting more...");
                }
            }

            diff
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(RunResult {
            output: response,
            session_id: Some(sid),
            duration_ms,
        })
    }

    /// Close and remove a session by ID.
    pub async fn close_session_inner(&self, session_id: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.remove(session_id) {
            // Kill on blocking thread to avoid blocking async runtime
            let _ = tokio::task::spawn_blocking(move || {
                session.kill().ok();
            })
            .await;
            tracing::info!(session_id = %session_id, "Session closed");
        }
    }
}

// --- Agent-specific output cleaning ---

impl AntigravityDriver {
    /// Strip agy-specific TUI chrome from cleaned output.
    /// Generic ANSI/stripping and spinner/border filtering is handled by
    /// the default clean_output() pipeline; this removes agy-specific
    /// text patterns.
    fn strip_agy_chrome(text: &str) -> String {
        text.lines()
            .filter(|line| {
                let trimmed = line.trim();

                // Skip agy header/logo lines (e.g., "✦ Antigravity CLI (gemini-2.5-pro)")
                if trimmed.starts_with('✦') || trimmed.starts_with("✧") {
                    return false;
                }

                // Skip footer/status lines that look like token counts or timing
                // (e.g., "Tokens used: 1,247 in / 382 out | Cost: $0.003")
                // (e.g., "Done in 2.3s")
                if trimmed.starts_with("Tokens used:")
                    || trimmed.starts_with("Tokens:")
                    || (trimmed.starts_with("Done in ") && trimmed.ends_with('s'))
                {
                    return false;
                }

                // Skip agy TUI status lines
                if trimmed == "Prioritizing Tool Usage"
                    || trimmed.starts_with("? for shortcuts")
                    || trimmed.starts_with("Gemini ")
                    || trimmed == ">"
                {
                    return false;
                }

                true
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    /// Compute the agent's response by extracting new content from screen diff.
    ///
    /// When subagents are involved, the TUI may rewrite the entire screen rather
    /// than appending lines. We handle this by:
    /// 1. Stripping ANSI from both before and after
    /// 2. Finding the first position where `after` differs from `before`
    /// 3. Taking everything from that position onward as the response
    /// 4. Running clean_output to clean TUI chrome
    fn compute_response_diff(&self, before: &str, after: &str) -> String {
        let before_clean = strip_ansi(before);
        let after_clean = strip_ansi(after);

        // If the screens are identical, there is no new content.
        if before_clean == after_clean {
            return String::new();
        }

        // If the screen was rewritten (after doesn't start with before),
        // or the screens are nearly identical, extract response from the full after text.
        let new_text = if let Some(pos) = after_clean.find(&before_clean) {
            // Before text found as substring — take everything after it
            after_clean[pos + before_clean.len()..].to_string()
        } else {
            // Screen was rewritten — take the full after text
            after_clean.clone()
        };

        let result = self.clean_output(&new_text);

        // If extraction produced nothing (e.g., all TUI chrome was stripped),
        // fall back to extracting from the full after text.
        if result.trim().is_empty() {
            self.clean_output(&after_clean)
        } else {
            result
        }
    }
}

#[async_trait::async_trait]
impl AgentDriver for AntigravityDriver {
    async fn initialize(&self) -> Result<()> {
        let settings = serde_json::json!({
            "model": self.model,
            "general": { "yolo": self.yolo },
            "mcpServers": self.mcp_servers.clone().unwrap_or(serde_json::Value::Null),
        });
        Self::write_agy_settings(&settings)?;
        Ok(())
    }

    fn clean_output(&self, raw: &str) -> String {
        // First apply the generic pipeline (strip ANSI + generic TUI chrome)
        let cleaned = output_parser::extract_response(raw);
        // Then strip agy-specific text patterns
        Self::strip_agy_chrome(&cleaned)
    }

    async fn run_prompt(&self, prompt: &str, session_id: Option<&str>) -> Result<RunResult> {
        if session_id.is_some() {
            return self.run_multi_turn(prompt, session_id).await;
        }

        // Single-turn: spawn agy -p
        let start = Instant::now();

        let mut cmd = Command::new("agy");
        cmd.arg("-p").arg(prompt).current_dir(&self.workspace);

        if self.yolo {
            cmd.arg("--dangerously-skip-permissions");
        }

        let output = cmd
            .output()
            .await
            .context("Failed to spawn agy — is it installed and in PATH?")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let duration_ms = start.elapsed().as_millis() as u64;

        if !output.status.success() {
            tracing::warn!(
                exit_code = ?output.status.code(),
                stderr = %stderr,
                "agy exited with error"
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

    async fn close_session(&self, session_id: &str) {
        self.close_session_inner(session_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_driver() -> AntigravityDriver {
        AntigravityDriver::new("/workspace".into(), false, "test-model".into(), None)
    }

    #[test]
    fn driver_new_initializes_empty_sessions() {
        let driver = make_driver();
        let sessions = driver.sessions.try_lock().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn compute_response_diff_appended_lines() {
        let before = "line1\nline2\nline3\n";
        let after = "line1\nline2\nline3\nThis is the agent response.\nSecond line.";
        let result = make_driver().compute_response_diff(before, after);
        assert!(result.contains("This is the agent response."));
        assert!(result.contains("Second line."));
    }

    #[test]
    fn compute_response_diff_rewrite() {
        let before = "aaa\nbbb\nccc";
        let after = "aaa\nNEW CONTENT\nccc";
        let result = make_driver().compute_response_diff(before, after);
        assert!(result.contains("NEW CONTENT"));
    }

    #[test]
    fn compute_response_diff_strips_tui_chrome() {
        let before = "old screen";
        let after = "old screen\n✦ Antigravity\nHere is my answer.\n──────────\n$ ";
        let result = make_driver().compute_response_diff(before, after);
        assert!(result.contains("Here is my answer."));
        // TUI chrome should be stripped
        assert!(!result.contains("✦"));
        assert!(!result.contains("───"));
    }

    #[test]
    fn compute_response_diff_empty_diff() {
        let text = "same content\nno change";
        let result = make_driver().compute_response_diff(text, text);
        assert!(result.is_empty() || result.trim().is_empty());
    }

    #[test]
    fn clean_output_strips_agy_header() {
        let driver = make_driver();
        let raw = "\u{2726} Antigravity CLI (test-model)\nHere is the answer.";
        let result = driver.clean_output(raw);
        assert!(!result.contains("Antigravity"));
        assert!(result.contains("Here is the answer."));
    }

    #[test]
    fn clean_output_strips_token_footer() {
        let driver = make_driver();
        let raw = "The answer.\nTokens used: 100 in / 50 out";
        let result = driver.clean_output(raw);
        assert!(result.contains("The answer."));
        assert!(!result.contains("Tokens used:"));
    }

    #[test]
    fn clean_output_strips_agy_tui_status() {
        let driver = make_driver();
        let raw = "Prioritizing Tool Usage\nGemini 3.1 Pro (High)\nReal response.";
        let result = driver.clean_output(raw);
        assert!(!result.contains("Prioritizing"));
        assert!(!result.contains("Gemini"));
        assert!(result.contains("Real response."));
    }

    #[test]
    fn clean_output_preserves_code_blocks() {
        let driver = make_driver();
        let raw = "\u{2726} Antigravity\n```python\ndef hello():\n    return \"world\"\n```\nDone in 1.0s";
        let result = driver.clean_output(raw);
        assert!(result.contains("```python"));
        assert!(result.contains("def hello():"));
        assert!(!result.contains("Antigravity"));
        assert!(!result.contains("Done in"));
    }
}
