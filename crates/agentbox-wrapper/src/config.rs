use std::collections::HashMap;
use std::env;

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub enum SessionMode {
    Single,
    Multi,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AppConfig {
    pub model: String,
    pub agent: String,
    pub port: u16,
    pub session_mode: SessionMode,
    pub yolo: bool,
    pub workspace: String,
    pub session_timeout_secs: u64,
    pub mcp_servers: Option<serde_json::Value>,
}

impl AppConfig {
    /// Parse config from any iterator of (key, value) pairs.
    /// Test-safe: does not touch process environment.
    pub fn from_vars(
        vars: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>,
    ) -> Result<Self> {
        let map: HashMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()))
            .collect();

        let model = map
            .get("AGENTBOX_MODEL")
            .cloned()
            .ok_or_else(|| anyhow!("AGENTBOX_MODEL environment variable is required"))?;

        let agent = map
            .get("AGENTBOX_AGENT")
            .cloned()
            .unwrap_or_else(|| "antigravity".into());

        let port = map
            .get("AGENTBOX_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(7080);

        let session_mode = match map
            .get("AGENTBOX_SESSION_MODE")
            .map(|s| s.as_str())
            .unwrap_or("single")
        {
            "multi" => SessionMode::Multi,
            _ => SessionMode::Single,
        };

        let yolo = map
            .get("AGENTBOX_YOLO")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        let workspace = map
            .get("AGENTBOX_WORKSPACE")
            .cloned()
            .unwrap_or_else(|| "/workspace".into());

        let session_timeout_secs = map
            .get("AGENTBOX_SESSION_TIMEOUT")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1800);

        let mcp_servers = map
            .get("AGENTBOX_MCP_SERVERS")
            .and_then(|s| serde_json::from_str(s).ok());

        Ok(Self {
            model,
            agent,
            port,
            session_mode,
            yolo,
            workspace,
            session_timeout_secs,
            mcp_servers,
        })
    }

    /// Parse config from process environment variables.
    /// NOTE: Not thread-safe for tests (env vars are process-global).
    pub fn from_env() -> Result<Self> {
        Self::from_vars(env::vars())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_vars() -> Vec<(&'static str, &'static str)> {
        vec![("AGENTBOX_MODEL", "gemini-2.5-pro")]
    }

    // --- from_vars: required fields ---

    #[test]
    fn missing_model_returns_error() {
        let result = AppConfig::from_vars(Vec::<(&str, &str)>::new());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("AGENTBOX_MODEL")
        );
    }

    #[test]
    fn minimal_config_parses() {
        let cfg = AppConfig::from_vars(minimal_vars()).unwrap();
        assert_eq!(cfg.model, "gemini-2.5-pro");
    }

    // --- from_vars: defaults ---

    #[test]
    fn defaults_are_applied() {
        let cfg = AppConfig::from_vars(minimal_vars()).unwrap();
        assert_eq!(cfg.agent, "antigravity");
        assert_eq!(cfg.port, 7080);
        assert_eq!(cfg.workspace, "/workspace");
        assert_eq!(cfg.session_timeout_secs, 1800);
        assert!(!cfg.yolo);
        assert!(cfg.mcp_servers.is_none());
        assert!(matches!(cfg.session_mode, SessionMode::Single));
    }

    // --- from_vars: boolean parsing ---

    #[test]
    fn yolo_true_variants() {
        for val in ["true", "1"] {
            let cfg = AppConfig::from_vars([
                ("AGENTBOX_MODEL", "m"),
                ("AGENTBOX_YOLO", val),
            ])
            .unwrap();
            assert!(cfg.yolo, "YOLO should be true for {val:?}");
        }
    }

    #[test]
    fn yolo_false_variants() {
        for val in ["false", "0", "yes", ""] {
            let cfg = AppConfig::from_vars([
                ("AGENTBOX_MODEL", "m"),
                ("AGENTBOX_YOLO", val),
            ])
            .unwrap();
            assert!(!cfg.yolo, "YOLO should be false for {val:?}");
        }
    }

    // --- from_vars: port parsing ---

    #[test]
    fn valid_port_is_parsed() {
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_PORT", "9090"),
        ])
        .unwrap();
        assert_eq!(cfg.port, 9090);
    }

    #[test]
    fn invalid_port_falls_back_to_default() {
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_PORT", "not-a-number"),
        ])
        .unwrap();
        assert_eq!(cfg.port, 7080);
    }

    // --- from_vars: session mode ---

    #[test]
    fn session_mode_multi() {
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_SESSION_MODE", "multi"),
        ])
        .unwrap();
        assert!(matches!(cfg.session_mode, SessionMode::Multi));
    }

    #[test]
    fn session_mode_unknown_defaults_to_single() {
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_SESSION_MODE", "whatever"),
        ])
        .unwrap();
        assert!(matches!(cfg.session_mode, SessionMode::Single));
    }

    // --- from_vars: MCP servers ---

    #[test]
    fn valid_mcp_servers_json_is_parsed() {
        let json = r#"{"fs":{"command":"npx","args":["-y","server"]}}"#;
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_MCP_SERVERS", json),
        ])
        .unwrap();
        let mcp = cfg.mcp_servers.expect("should be Some");
        assert!(mcp.get("fs").is_some());
    }

    #[test]
    fn invalid_mcp_servers_json_is_ignored() {
        let cfg = AppConfig::from_vars([
            ("AGENTBOX_MODEL", "m"),
            ("AGENTBOX_MCP_SERVERS", "not json"),
        ])
        .unwrap();
        assert!(cfg.mcp_servers.is_none());
    }

}
