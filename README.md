# agentbox

[中文](README.cn.md)

**Containerized runtime for CLI-based AI coding agents.**

Wraps CLI agents like Antigravity CLI (agy), Claude Code, Codex, and GitHub Copilot inside Docker containers, exposing them as MCP and ACP servers for other agents to call.

## Why agentbox?

Take Antigravity CLI as an example — it has a critical pain point: **no way to specify models in non-interactive mode**.

`agy -p "prompt"` is one-shot only. Session continuation flags (`-c`/`--continue`) only work in interactive TUI mode. Model selection requires modifying `settings.json`. When multiple agy instances need different models concurrently, they fight over the shared config file.

Containerization solves all of this: each container has its own filesystem, its own `settings.json`, and its own isolated agent process.

## Quick Start

### Prerequisites

- Docker and docker-compose
- Rust toolchain (for local development)

### Build

```bash
# Compile the wrapper binary
cargo build --release

# Build the Docker image
cp target/release/agentbox-wrapper docker/
docker build -t agentbox/antigravity -f docker/Dockerfile docker/
```

### Run the Demo

Use `docker-compose.yml` to start two agy instances with different models simultaneously:

```yaml
services:
  agy-pro:
    image: agentbox/antigravity
    environment:
      AGENTBOX_MODEL: gemini-2.5-pro
      AGENTBOX_YOLO: "true"
    volumes:
      - ./proj1:/workspace
    ports:
      - "9091:7080"

  agy-flash:
    image: agentbox/antigravity
    environment:
      AGENTBOX_MODEL: gemini-2.5-flash
      AGENTBOX_YOLO: "true"
    volumes:
      - ./proj2:/workspace
    ports:
      - "9092:7080"
```

```bash
docker compose -f docker/docker-compose.yml up
```

### Usage

**ACP (HTTP REST):**

```bash
# Discover agents
curl http://localhost:9091/agents

# Single-turn run
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"explain this project"}]}]
  }'

# Multi-turn: first message (gets session_id back)
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"analyze the auth module"}]}],
    "session_id": "my-session-1"
  }'

# Multi-turn: follow-up (same session)
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"refactor it to use JWT"}]}],
    "session_id": "my-session-1"
  }'

# Close a session
curl -X DELETE http://localhost:9091/runs/run_abc123
```

**MCP (JSON-RPC over HTTP):**

In any MCP client (QoderWork, Claude Code, Cursor, etc.), add:

```
Server URL: http://localhost:9091/mcp
```

Then call the `run_agent` tool:

```
run_agent(prompt: "explain this project")
run_agent(prompt: "continue optimizing", session_id: "my-session-1")
```

**Hermes Agent:**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) can discover and call agentbox containers as MCP tools:

```bash
# Add an agentbox MCP server
hermes mcp add agentbox-flash --url http://localhost:9091/mcp
hermes mcp add agentbox-pro   --url http://localhost:9092/mcp
hermes mcp add agentbox-opus  --url http://localhost:9093/mcp

# List configured servers
hermes mcp list

# Remove a server
hermes mcp remove agentbox-flash

# Test connectivity
hermes mcp test agentbox-flash
```

Each server's `tools/list` response includes the model name (e.g. `AgentBox — Gemini 3.1 Pro (High)`) in the tool title and description, so the LLM can choose the appropriate model for each task.

---

## Configuration

All configuration is injected through environment variables. No host-side config files to manage.

| Variable | Default | Description |
|----------|---------|-------------|
| `AGENTBOX_MODEL` | *(required)* | Model name, e.g. `gemini-2.5-pro` |
| `AGENTBOX_AGENT` | `antigravity` | Agent type. A [driver factory](#architecture) selects the matching implementation. Supported: `antigravity` |
| `AGENTBOX_PORT` | `7080` | HTTP port (shared by MCP + ACP) |
| `AGENTBOX_SESSION_MODE` | `single` | `single` (one-shot) or `multi` (conversational) |
| `AGENTBOX_YOLO` | `false` | Auto-approve all agent tool calls |
| `AGENTBOX_MCP_SERVERS` | *(empty)* | MCP servers to inject into the agent (JSON) |
| `AGENTBOX_WORKSPACE` | `/workspace` | Working directory inside the container |
| `AGENTBOX_SESSION_TIMEOUT` | `1800` | Multi-turn session idle timeout in seconds |

### Example

```bash
docker run -d \
  -e AGENTBOX_MODEL=gemini-2.5-pro \
  -e AGENTBOX_YOLO=true \
  -e AGENTBOX_MCP_SERVERS='{"filesystem":{"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/workspace"]}}' \
  -v $(pwd)/my-project:/workspace \
  -p 7080:7080 \
  agentbox/antigravity
```

---

## API Reference

### ACP Endpoints

```
GET    /agents          List available agents
POST   /runs            Create a run (optional session_id for multi-turn)
GET    /runs/:id        Get run status/result
DELETE /runs/:id        Close run and its associated session
```

**POST /runs request:**

```json
{
  "agent_name": "antigravity",
  "input": [
    {
      "role": "user",
      "parts": [{"type": "text", "text": "your prompt here"}]
    }
  ],
  "session_id": "optional, for multi-turn conversations"
}
```

**POST /runs response:**

```json
{
  "run_id": "run_abc123",
  "session_id": "sess_xyz789",
  "status": "completed",
  "output": [
    {
      "role": "agent",
      "parts": [{"type": "text", "text": "The agent's response..."}]
    }
  ]
}
```

### MCP Tool

```json
{
  "name": "run_agent",
  "inputSchema": {
    "properties": {
      "prompt":     { "type": "string", "description": "The prompt to send to the agent" },
      "session_id": { "type": "string", "description": "Optional session ID for multi-turn" }
    },
    "required": ["prompt"]
  }
}
```

---

## Architecture

```
┌─── Host ──────────────────────────────────────────────┐
│                                                        │
│  docker run / docker-compose                           │
│       │                                                │
│       ▼                                                │
│  ┌─── Container ────────────────────────────────────┐  │
│  │                                                   │  │
│  │  agentbox-wrapper (single Rust binary)             │  │
│  │                                                   │  │
│  │  ┌──────────────┐  ┌───────────────────────────┐ │  │
│  │  │Config Injector│  │ Driver Factory            │ │  │
│  │  │              │  │                           │ │  │
│  │  │ env vars ──► │  │  AGENTBOX_AGENT ──►       │ │  │
│  │  │ settings.json│  │  antigravity driver       │ │  │
│  │  └──────────────┘  │                           │ │  │
│  │                    │  Single: agy -p "..."    │ │  │
│  │                    │  Multi: PTY + VT100       │ │  │
│  │                    └───────────────────────────┘ │  │
│  │                                                   │  │
│  │  ┌──────────────┐                                 │  │
│  │  │ MCP Server   │  (rmcp, JSON-RPC)               │  │
│  │  ├──────────────┤                                 │  │
│  │  │ ACP Server   │  (axum, REST)                   │  │
│  │  └──────────────┘                                 │  │
│  │                                                   │  │
│  │  /workspace  ←── host volume mount                │  │
│  └───────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────┘
```

### Tech Stack

| Component | Technology |
|-----------|------------|
| Language | Rust (full-stack) |
| Async runtime | tokio |
| HTTP framework | axum 0.8 |
| MCP SDK | rmcp 1.7 |
| PTY | portable-pty 0.9 |
| Terminal emulation | vt100 0.16 |
| Serialization | serde / serde_json |
| Logging | tracing |

### Project Structure

```
agentbox/
├── Cargo.toml                       # workspace root
├── crates/
│   └── agentbox-wrapper/
│       └── src/
│           ├── main.rs              # entry: read env → generate config → start servers
│           ├── config.rs            # env var parsing
│           ├── mcp.rs               # MCP server (rmcp)
│           ├── acp.rs               # ACP REST endpoints (axum)
│           ├── session.rs           # PTY session management (portable-pty + vt100)
│           ├── output_parser.rs     # ANSI stripping + TUI chrome filtering
│           └── drivers/
│               ├── mod.rs           # AgentDriver trait + driver factory
│               └── antigravity.rs   # agy driver (single-turn + multi-turn, config init, TUI cleaning)
├── docker/
│   ├── Dockerfile
│   └── docker-compose.yml
├── DESIGN.md                        # detailed design document
├── README.cn.md                     # 中文版本
└── README.md                        # this file
```

---

## Development

```bash
# Run all tests
cargo test

# Clippy lint check
cargo clippy

# Release build
cargo build --release

# Run locally with debug logging
AGENTBOX_MODEL=gemini-2.5-pro RUST_LOG=debug cargo run
```

### Test Coverage

80 unit tests covering: config parsing (13), ACP endpoints (11), MCP server (3), agent driver (6), PTY sessions (7), output parsing (40).

---

## Roadmap

- [x] **Phase 1** — Single-turn MVP: config injection, agy -p driver, MCP + ACP endpoints
- [x] **Phase 2** — Multi-turn support: PTY session management, screen stability detection, session lifecycle
- [x] **Phase 2.5** — ACP streaming responses (SSE)
- [ ] **Phase 3** — MCP stdio transport, error recovery improvements
- [ ] **Phase 4** — More agent drivers (Claude Code, Codex, GitHub Copilot) — *driver factory architecture ready, adding a new driver only requires implementing the AgentDriver trait + one match arm*

---

## License

MIT
