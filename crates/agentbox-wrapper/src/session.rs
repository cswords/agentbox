use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};
use vt100::Parser;

/// A managed PTY session running an interactive CLI agent.
///
/// Spawns the agent in a pseudo-terminal with VT100 emulation.
/// A background thread continuously drains PTY output into the parser.
/// The caller injects prompts and waits for screen stability to detect
/// response completion.
#[allow(dead_code)]
pub struct PtySession {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    parser: Arc<Mutex<Parser>>,
    alive: Arc<Mutex<bool>>,
    cols: u16,
    rows: u16,
}

impl PtySession {
    /// Spawn a new PTY session running the given command.
    /// If `cwd` is provided, the command runs in that directory.
    #[allow(dead_code)]
    pub fn new(command: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self> {
        Self::with_cwd(command, args, cols, rows, None)
    }

    /// Spawn a new PTY session with an optional working directory.
    pub fn with_cwd(
        command: &str,
        args: &[&str],
        cols: u16,
        rows: u16,
        cwd: Option<&str>,
    ) -> Result<Self> {
        let pty_system = NativePtySystem::default();

        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("Failed to open PTY")?;

        let mut cmd = CommandBuilder::new(command);
        for arg in args {
            cmd.arg(arg);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }

        let raw_child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn command in PTY")?;

        let master = Arc::new(Mutex::new(pair.master));
        let child = Arc::new(Mutex::new(raw_child));
        let parser = Arc::new(Mutex::new(Parser::new(rows, cols, 0)));
        let alive = Arc::new(Mutex::new(true));

        // Clone reader and take writer through the MutexGuard (both take &self).
        // take_writer can only be called once — it removes the writer from the master.
        let (reader, writer) = {
            let m = master.lock().unwrap();
            let reader = m.try_clone_reader().context("Failed to clone PTY reader")?;
            let writer = m.take_writer().context("Failed to take PTY writer")?;
            (reader, writer)
        };
        let writer = Arc::new(Mutex::new(writer));

        // Spawn a background thread to continuously drain PTY output

        let parser_clone = parser.clone();
        let alive_clone = alive.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF — child has closed the PTY
                        *alive_clone.lock().unwrap() = false;
                        break;
                    }
                    Ok(n) => {
                        parser_clone.lock().unwrap().process(&buf[..n]);
                    }
                    Err(_) => {
                        *alive_clone.lock().unwrap() = false;
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master,
            writer,
            child,
            parser,
            alive,
            cols,
            rows,
        })
    }

    /// Inject text into the PTY (simulates typing).
    /// Appends \r (CR) to simulate pressing Enter in raw mode terminals.
    pub fn inject_prompt(&self, text: &str) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        let data = format!("{text}\r");
        writer
            .write_all(data.as_bytes())
            .context("Failed to write to PTY")?;
        writer.flush().ok();
        Ok(())
    }

    /// Write raw bytes to the PTY without appending anything.
    /// Use for escape sequences (arrow keys, etc.) in raw mode terminals.
    pub fn write_raw(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer
            .write_all(data)
            .context("Failed to write raw bytes to PTY")?;
        writer.flush().ok();
        Ok(())
    }

    /// Write raw bytes to the PTY without appending anything.
    /// Use for escape sequences (arrow keys, etc.) in raw mode terminals.
    ///
    /// Wait until the screen content stops changing.
    ///
    /// Polls the screen hash every `poll_interval` and returns when
    /// the hash hasn't changed for `settle_duration`.
    pub async fn wait_for_stable(
        &self,
        settle_duration: Duration,
        poll_interval: Duration,
    ) -> Result<String> {
        let parser = self.parser.clone();
        let alive = self.alive.clone();

        tokio::task::spawn_blocking(move || {
            let mut last_hash: u64 = 0;
            let mut last_change = Instant::now();

            loop {
                if !*alive.lock().unwrap() {
                    // Process died — return whatever we have
                    let text = parser.lock().unwrap().screen().contents();
                    return Ok(text);
                }

                // Small sleep to avoid busy-waiting
                std::thread::sleep(poll_interval);

                let text = parser.lock().unwrap().screen().contents();
                let hash = crate::output_parser::screen_hash(&text);

                if hash != last_hash {
                    last_hash = hash;
                    last_change = Instant::now();
                } else if last_change.elapsed() >= settle_duration {
                    // Screen has been stable long enough
                    return Ok(text);
                }
            }
        })
        .await
        .context("PTY stability task panicked")?
    }

    /// Wait until the screen content stops changing (synchronous version).
    /// Suitable for use inside `spawn_blocking` or other non-async contexts.
    pub fn wait_for_stable_sync(
        &self,
        settle_duration: Duration,
        poll_interval: Duration,
    ) -> Result<String> {
        let mut last_hash: u64 = 0;
        let mut last_change = Instant::now();

        loop {
            if !*self.alive.lock().unwrap() {
                let text = self.parser.lock().unwrap().screen().contents();
                return Ok(text);
            }

            std::thread::sleep(poll_interval);

            let text = self.parser.lock().unwrap().screen().contents();
            let hash = crate::output_parser::screen_hash(&text);

            if hash != last_hash {
                last_hash = hash;
                last_change = Instant::now();
            } else if last_change.elapsed() >= settle_duration {
                return Ok(text);
            }
        }
    }

    /// Get the current screen text.
    pub fn screen_text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// Get the current screen hash for change detection.
    #[allow(dead_code)]
    pub fn screen_hash(&self) -> u64 {
        let text = self.screen_text();
        crate::output_parser::screen_hash(&text)
    }

    /// Whether the child process is still alive.
    pub fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    /// Terminal dimensions.
    #[allow(dead_code)]
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Kill the child process and clean up.
    pub fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().unwrap();
        child.kill().ok();
        *self.alive.lock().unwrap() = false;
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.kill().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a simple shell command instead of agy for unit tests
    const TEST_CMD: &str = "cat";
    const TEST_ARGS: &[&str] = &[];

    #[test]
    fn pty_session_spawns_and_is_alive() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 80, 24).unwrap();
        assert!(session.is_alive());
    }

    #[test]
    fn pty_session_reports_size() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 120, 40).unwrap();
        assert_eq!(session.size(), (120, 40));
    }

    #[test]
    fn pty_session_inject_and_read() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 80, 24).unwrap();

        // `cat` echoes input back to the terminal
        session.inject_prompt("hello world").unwrap();

        // Give the background reader thread time to process
        std::thread::sleep(Duration::from_millis(200));

        let text = session.screen_text();
        assert!(
            text.contains("hello world"),
            "Expected 'hello world' in screen, got: {text:?}"
        );
    }

    #[tokio::test]
    async fn pty_session_wait_for_stable() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 80, 24).unwrap();

        session.inject_prompt("line one").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        session.inject_prompt("line two").unwrap();

        let text = session
            .wait_for_stable(Duration::from_millis(300), Duration::from_millis(50))
            .await
            .unwrap();

        assert!(text.contains("line one"));
        assert!(text.contains("line two"));
    }

    #[test]
    fn pty_session_kill_marks_dead() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 80, 24).unwrap();
        assert!(session.is_alive());

        session.kill().unwrap();
        // Give the reader thread time to detect EOF
        std::thread::sleep(Duration::from_millis(100));

        assert!(!session.is_alive());
    }

    #[test]
    fn pty_session_screen_hash_changes() {
        let session = PtySession::new(TEST_CMD, TEST_ARGS, 80, 24).unwrap();
        let hash_before = session.screen_hash();

        session.inject_prompt("something").unwrap();
        std::thread::sleep(Duration::from_millis(200));

        let hash_after = session.screen_hash();
        assert_ne!(hash_before, hash_after);
    }

    #[test]
    fn pty_session_with_echo_command() {
        // Use `sh -c` to run a command that outputs and waits
        let session = PtySession::new(
            "sh",
            &["-c", "echo 'agent ready'; cat"],
            80,
            24,
        )
        .unwrap();

        // Wait for the initial output
        std::thread::sleep(Duration::from_millis(300));
        let text = session.screen_text();
        assert!(
            text.contains("agent ready"),
            "Expected 'agent ready' in: {text:?}"
        );
    }
}
