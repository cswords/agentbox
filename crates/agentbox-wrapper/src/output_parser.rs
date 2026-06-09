//! Parse and clean TUI output from CLI agents.
//!
//! Handles ANSI escape codes, generic TUI chrome (spinners, borders),
//! and extracts the agent's actual response text.
//! Agent-specific formatting is handled by each driver's clean_output().

/// Strip all ANSI escape sequences from a string.
pub fn strip_ansi(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // ESC — start of an escape sequence
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ <params> <final byte>
                    chars.next(); // consume '['
                    // Skip parameter bytes (0x30-0x3F), intermediate bytes (0x20-0x2F),
                    // and the final byte (0x40-0x7E)
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if (0x40..=0x7E).contains(&(next as u32)) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] ... ST (ST = ESC \ or BEL)
                    chars.next(); // consume ']'
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next == '\x07' {
                            // BEL terminates OSC
                            break;
                        }
                        if next == '\x1b' {
                            // ESC \ terminates OSC
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some('(') | Some(')') => {
                    // Character set selection: ESC ( B, ESC ) 0, etc.
                    chars.next(); // consume '(' or ')'
                    chars.next(); // consume the designator byte
                }
                _ => {
                    // Other single-char ESC sequences (ESC M, ESC D, etc.)
                    chars.next();
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Remove common generic TUI chrome lines.
///
/// Strips:
/// - Progress spinners / status lines (⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏, ⣾⣽⣻⢿⡿⣟⣯⣷, etc.)
/// - Separator lines (horizontal rules)
/// - Lines that are purely decorative borders
///
/// Agent-specific text patterns (e.g. "✦ Antigravity", "Tokens used:") are
/// handled by each driver's `clean_output()` override.
pub fn strip_tui_chrome(text: &str) -> String {
    let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];

    text.lines()
        .filter(|line| {
            let trimmed = line.trim();

            // Skip spinner lines
            if let Some(first_char) = trimmed.chars().next() {
                if spinner_chars.contains(&first_char) {
                    return false;
                }
            }

            // Skip pure separator/border lines
            if !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|c| matches!(c, '─' | '═' | '━' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍'))
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

/// Extract the agent's response text from raw output.
///
/// Pipeline: strip ANSI → strip generic TUI chrome → trim.
/// Agent-specific formatting is handled by each driver's clean_output().
pub fn extract_response(raw_output: &str) -> String {
    let no_ansi = strip_ansi(raw_output);
    strip_tui_chrome(&no_ansi)
}

/// Detect whether a TUI screen indicates the agent has finished responding.
///
/// Heuristics:
/// - Screen contains a prompt indicator (e.g., "> ", "❯ ")
/// - Screen contains a mode indicator (e.g., "Ready")
#[allow(dead_code)]
pub fn looks_like_prompt(text: &str) -> bool {
    let last_line = text.lines().last().unwrap_or("").trim();

    // Common prompt indicators
    if last_line.starts_with('>')
        || last_line.starts_with("❯")
        || last_line.starts_with("➜")
        || last_line.ends_with('$')
        || last_line.ends_with("# ")
    {
        return true;
    }

    // agent-specific: often shows a mode indicator or "Ready" at bottom
    if last_line.contains("Ready") || last_line.contains("(esc)") {
        return true;
    }

    false
}

/// Compute a simple hash of visible text content for change detection.
/// Uses FNV-1a 64-bit hash.
pub fn screen_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── ANSI stripping ───────────────────────────────────────

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn empty_string_passes_through() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn sgr_color_codes_stripped() {
        // ESC[31m = red, ESC[0m = reset
        let input = "\x1b[31merror:\x1b[0m something went wrong";
        assert_eq!(strip_ansi(input), "error: something went wrong");
    }

    #[test]
    fn sgr_bold_and_underline_stripped() {
        // ESC[1m = bold, ESC[4m = underline
        let input = "\x1b[1m\x1b[4mimportant\x1b[0m text";
        assert_eq!(strip_ansi(input), "important text");
    }

    #[test]
    fn sgr_256_color_stripped() {
        // ESC[38;5;196m = 256-color red foreground
        let input = "\x1b[38;5;196mbright red\x1b[0m";
        assert_eq!(strip_ansi(input), "bright red");
    }

    #[test]
    fn sgr_24bit_rgb_stripped() {
        // ESC[38;2;255;0;0m = 24-bit RGB red
        let input = "\x1b[38;2;255;0;0mtrue color\x1b[0m";
        assert_eq!(strip_ansi(input), "true color");
    }

    #[test]
    fn cursor_movement_stripped() {
        // ESC[2J = clear screen, ESC[H = cursor home, ESC[5;10H = move to row 5 col 10
        let input = "\x1b[2J\x1b[H\x1b[5;10Htext at position";
        assert_eq!(strip_ansi(input), "text at position");
    }

    #[test]
    fn cursor_show_hide_stripped() {
        // ESC[?25l = hide cursor, ESC[?25h = show cursor
        let input = "\x1b[?25linvisible\x1b[?25h visible";
        assert_eq!(strip_ansi(input), "invisible visible");
    }

    #[test]
    fn osc_title_stripped_with_bel() {
        // OSC sequence: set terminal title, terminated by BEL (\x07)
        let input = "\x1b]0;My Terminal Title\x07actual content";
        assert_eq!(strip_ansi(input), "actual content");
    }

    #[test]
    fn osc_title_stripped_with_esc_backslash() {
        // OSC sequence terminated by ESC \
        let input = "\x1b]0;Title\x1b\\content here";
        assert_eq!(strip_ansi(input), "content here");
    }

    #[test]
    fn charset_selection_stripped() {
        // ESC(B = select ASCII charset, ESC)0 = select line drawing charset
        let input = "\x1b(Bnormal\x1b)0text";
        assert_eq!(strip_ansi(input), "normaltext");
    }

    #[test]
    fn multiple_sequences_in_one_line() {
        // Simulates a realistic colored line: "✓ Updated file.txt"
        let input = "\x1b[32m✓\x1b[0m \x1b[1mUpdated\x1b[0m file.txt";
        assert_eq!(strip_ansi(input), "✓ Updated file.txt");
    }

    #[test]
    fn multiline_ansi_output() {
        let input = "\x1b[36mModel: gemini-2.5-pro\x1b[0m\n\x1b[33mThinking...\x1b[0m\nHere is my answer.\n\x1b[90mTokens: 42\x1b[0m";
        let result = strip_ansi(input);
        assert!(result.contains("Model: gemini-2.5-pro"));
        assert!(result.contains("Thinking..."));
        assert!(result.contains("Here is my answer."));
        assert!(result.contains("Tokens: 42"));
        assert!(!result.contains("\x1b"));
    }

    // ─── TUI chrome stripping ─────────────────────────────────

    #[test]
    fn spinner_lines_removed() {
        let input = "⠋ Thinking...\n⠙ Analyzing code...\nThe actual response.";
        let result = strip_tui_chrome(input);
        assert_eq!(result, "The actual response.");
        assert!(!result.contains("Thinking"));
    }

    #[test]
    fn all_spinner_variants_removed() {
        let spinners = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        for s in spinners {
            let input = format!("{s} Loading...\nreal content");
            let result = strip_tui_chrome(&input);
            assert!(
                !result.contains("Loading"),
                "spinner {s} was not stripped"
            );
            assert!(result.contains("real content"));
        }
    }

    #[test]
    fn braille_spinner_variants_removed() {
        let spinners = ['⣾', '⣽', '⣻', '⢿', '⡿', '⣟', '⣯', '⣷'];
        for s in spinners {
            let input = format!("{s} Processing...\nresult");
            let result = strip_tui_chrome(&input);
            assert!(
                !result.contains("Processing"),
                "braille spinner {s} was not stripped"
            );
        }
    }

    #[test]
    fn separator_lines_removed() {
        let separators = ["────────", "════════", "━━━━━━━━", "┄┄┄┄┄┄┄┄", "╌╌╌╌╌╌╌╌"];
        for sep in separators {
            let input = format!("before\n{sep}\nafter");
            let result = strip_tui_chrome(&input);
            assert!(result.contains("before"));
            assert!(result.contains("after"));
            assert!(!result.contains(sep), "separator {sep} was not stripped");
        }
    }

    #[test]
    fn content_with_box_drawing_preserved() {
        // Lines that contain box drawing chars BUT also have text should be kept
        let input = "│ This is in a box │\n┌─────────────┐\nRegular text";
        let result = strip_tui_chrome(input);
        assert!(result.contains("│ This is in a box │"));
        assert!(result.contains("Regular text"));
    }

    #[test]
    fn internal_blank_lines_preserved() {
        let input = "first paragraph\n\nsecond paragraph";
        let result = strip_tui_chrome(input);
        assert!(result.contains("first paragraph\n\nsecond paragraph"));
    }

    // ─── Full pipeline (extract_response) ─────────────────────

    #[test]
    fn clean_output_passes_through_unchanged() {
        let input = "Simple response with no formatting.\nJust plain text.";
        assert_eq!(extract_response(input), input);
    }

    #[test]
    fn output_with_only_spinner_and_response() {
        let raw = "\x1b[33m⠋\x1b[0m Working on it...\n\nDone! Created 3 files.";
        let result = extract_response(raw);
        assert_eq!(result, "Done! Created 3 files.");
    }

    // ─── Prompt detection ─────────────────────────────────────

    #[test]
    fn detects_gt_prompt() {
        assert!(looks_like_prompt("some output\n> "));
    }

    #[test]
    fn detects_fancy_prompt() {
        assert!(looks_like_prompt("some output\n❯ "));
    }

    #[test]
    fn detects_arrow_prompt() {
        assert!(looks_like_prompt("some output\n➜ "));
    }

    #[test]
    fn detects_dollar_prompt() {
        assert!(looks_like_prompt("some output\nuser@host $ "));
    }

    #[test]
    fn detects_ready_indicator() {
        assert!(looks_like_prompt("some output\nReady"));
    }

    #[test]
    fn detects_esc_indicator() {
        assert!(looks_like_prompt("some output\nPress (esc) to cancel"));
    }

    #[test]
    fn does_not_detect_regular_text() {
        assert!(!looks_like_prompt("Just some regular text output"));
    }

    #[test]
    fn does_not_detect_empty() {
        assert!(!looks_like_prompt(""));
    }

    // ─── Screen hashing ───────────────────────────────────────

    #[test]
    fn same_text_same_hash() {
        let text = "The quick brown fox";
        assert_eq!(screen_hash(text), screen_hash(text));
    }

    #[test]
    fn different_text_different_hash() {
        assert_ne!(screen_hash("hello"), screen_hash("world"));
    }

    #[test]
    fn empty_string_has_consistent_hash() {
        assert_eq!(screen_hash(""), screen_hash(""));
    }

    #[test]
    fn hash_changes_with_content() {
        let screen1 = "Loading...\nProcessing file 1/5";
        let screen2 = "Loading...\nProcessing file 2/5";
        assert_ne!(screen_hash(screen1), screen_hash(screen2));
    }

    // ─── Edge cases ───────────────────────────────────────────

    #[test]
    fn handles_only_ansi_codes() {
        let input = "\x1b[31m\x1b[0m\x1b[1m\x1b[0m";
        assert_eq!(strip_ansi(input), "");
    }

    #[test]
    fn handles_truncated_escape_sequence() {
        // ESC at end of string with no following byte
        let input = "text\x1b";
        let result = strip_ansi(input);
        assert_eq!(result, "text");
    }

    #[test]
    fn handles_unicode_content_with_ansi() {
        let input = "\x1b[32m日本語テスト\x1b[0m \x1b[1m中文\x1b[0m 한국어";
        assert_eq!(strip_ansi(input), "日本語テスト 中文 한국어");
    }
}
