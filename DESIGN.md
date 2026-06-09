# agentbox — Design Document

## What is agentbox?

agentbox is a containerized runtime for CLI-based AI coding agents. It wraps agents like Antigravity CLI (agy), Claude Code, Codex, and Github Copilot inside Docker containers, injects configuration (model, MCP tools, skills), mounts host working directories, and exposes the agents as MCP and ACP servers — so other agents can call them as tools or peers.

### Why start with Antigravity CLI?

agy has a critical limitation: **no way to specify models in non-interactive mode**.

- `agy -p "prompt"` is one-shot only (no session continuation)
- `-c` / `--continue` / `--conversation` only work in interactive TUI mode
- Model selection requires modifying `settings.json` — the only method available
- When multiple agy instances need different models concurrently, they fight over the shared config file

Containerization solves all of this: each container has its own filesystem, its own `settings.json`, and its own isolated agy process.

---

## Architecture

### Design principles

- **Docker is the CLI.** No custom host-side tooling. Container lifecycle is managed with standard `docker run` or `docker-compose`.
- **Don't reinvent wheels.** Use existing MCP/ACP clients (QoderWork, Claude Code, Cursor, curl) for interaction. No custom demo clients.
- **One binary per container.** `agentbox-wrapper` is the sole process inside the container. It reads environment variables, generates agent config, and runs MCP + ACP servers.

### High-level

```
┌─── Host ──────────────────────────────────────────────────────┐
│                                                                │
│  docker run / docker-compose                                   │
│  (standard Docker tooling — no custom CLI)                     │
│       │                                                        │
│       ▼                                                        │
│  ┌─── Container ────────────────────────────────────────────┐  │
│  │                                                           │  │
│  │  agentbox-wrapper (single Rust binary)                    │  │
│  │                                                           │  │
│  │  ┌─────────────────┐  ┌───────────────────────────────┐  │  │
│  │  │  Config Injector │  │  Agent Driver                 │  │  │
│  │  │                  │  │                               │  │  │
│  │  │  env vars ──►    │  │  ┌─ single-turn ──────────┐  │  │  │
│  │  │  settings.json   │  │  │ Command::spawn         │  │  │  │
│  │  │                  │  │  │ agy -p "..."           │  │  │  │
│  │  └─────────────────┘  │  └────────────────────────┘  │  │  │
│  │                        │                               │  │  │
│  │  ┌─────────────────┐  │  ┌─ multi-turn ───────────┐  │  │  │
│  │  │  MCP Server     │  │  │ PTY + VT100 emulator   │  │  │  │
│  │  │  (rmcp, :7080)  │  │  │ interactive agy        │  │  │  │
│  │  ├─────────────────┤  │  └────────────────────────┘  │  │  │
│  │  │  ACP Server     │  │                               │  │  │
│  │  │  (axum, :7080)  │  │                               │  │  │
│  │  └─────────────────┘  └───────────────────────────────┘  │  │
│  │                                                           │  │
│  │  /workspace  ←── volume mount from host                   │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                │
│  MCP clients (QoderWork / Claude Code / Cursor / ...)         │
│  ACP clients (curl / any HTTP client)                          │
│       │                                                        │
│       └──── connect to :7080 ────►                             │
└────────────────────────────────────────────────────────────────┘
```

### Single binary

**`agentbox-wrapper`** — runs inside the container as the entrypoint.

Startup sequence:

1. Read environment variables (`AGENTBOX_MODEL`, `AGENTBOX_MCP_SERVERS`, `AGENTBOX_SESSION_MODE`, etc.)
2. Generate agent-specific config (e.g., `settings.json` for agy)
3. Start MCP server (rmcp) and ACP server (axum) on a shared HTTP port
4. Wait for incoming MCP/ACP requests
5. Route each request to the appropriate agent driver

### Cargo workspace structure

```
agentbox/
├── Cargo.toml                    # workspace root
├── crates/
│   └── agentbox-wrapper/         # the only binary crate
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs            # entry: read env → generate config → start servers
│           ├── config.rs          # env var parsing + agy settings.json generation
│           ├── mcp.rs             # MCP server (rmcp)
│           ├── acp.rs             # ACP REST endpoints (axum)
│           ├── session.rs         # PTY session manager (portable-pty + vt100)
│           ├── output_parser.rs   # ANSI stripping + TUI chrome removal
│           └── drivers/
│               ├── mod.rs         # AgentDriver trait
│               └── antigravity.rs # agy-specific driver
│
├── docker/
│   ├── Dockerfile                 # agy container image
│   └── docker-compose.yml         # demo: multi-model setup
│
├── DESIGN.md                      # this file
└── README.md
```

---

## Configuration (via environment variables)

All configuration is injected through environment variables at container startup. No custom CLI, no config files to manage on the host.

| Variable | Default | Description |
|----------|---------|-------------|
| `AGENTBOX_MODEL` | *(required)* | Model name (e.g., `gemini-2.5-pro`) |
| `AGENTBOX_AGENT` | `antigravity` | Agent type (future: `claude-code`, `codex`, etc.) |
| `AGENTBOX_PORT` | `7080` | HTTP port for MCP + ACP |
| `AGENTBOX_SESSION_MODE` | `single` | `single` (one-shot) or `multi` (conversational) |
| `AGENTBOX_YOLO` | `false` | Auto-approve all agent tool calls |
| `AGENTBOX_MCP_SERVERS` | *(empty)* | JSON object defining MCP servers to inject into the agent |
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

## Agent Driver: Antigravity CLI (agy)

### Configuration injection

agy reads its configuration from `~/.gemini/antigravity-cli/settings.json`. The wrapper generates this file at startup from environment variables:

```json
{
  "model": {
    "name": "gemini-2.5-pro"
  },
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
    }
  },
  "general": {
    "yolo": true
  }
}
```

The wrapper writes this file before any agy process is spawned.

### Single-turn mode (default)

For one-shot tasks. The wrapper spawns `agy -p "<prompt>"`, captures stdout, and returns the result.

```
MCP/ACP request
    │
    ▼
Command::new("agy")
    .arg("-p").arg(prompt)
    .current_dir("/workspace")
    .output()
    │
    ▼
Parse stdout → Protocol response
```

**Pros**: Simple, stateless, fast startup.
**Cons**: No conversation history. Each call is a fresh session.

### Multi-turn mode (session)

For conversational interactions. The wrapper spawns an interactive `agy` process bound to a PTY and keeps it alive for the duration of the session.

```
Session created
    │
    ▼
PTY::spawn("agy", cols=120, rows=40)
    │
    ▼
Wait for agy ready indicator (prompt appears)
    │
    ▼
┌─ MCP/ACP request #1 ──────────────────┐
│                                         │
│  Inject prompt via PTY stdin            │
│       │                                 │
│       ▼                                 │
│  VT100 emulator processes output        │
│       │                                 │
│       ▼                                 │
│  Detect "response complete" indicator   │
│  (prompt reappears / stable screen)     │
│       │                                 │
│       ▼                                 │
│  Extract text → Protocol response       │
│                                         │
└─────────────────────────────────────────┘
    │
    ▼
┌─ MCP/ACP request #2 ──────────────────┐
│  (same cycle, same PTY session)        │
└─────────────────────────────────────────┘
    │
    ▼
Session ended → Kill PTY process
```

**Key challenges for multi-turn**:

1. **Detecting when agy is ready**: Watch for the prompt indicator in the TUI output.
2. **Detecting response completion**: Screen stability detection — if the screen hash hasn't changed for N ms, the response is complete.
3. **Extracting clean text**: Strip ANSI escape codes and TUI chrome (headers, footers, status bars).
4. **Error recovery**: Detect dead PTY and report error to the caller.

### PTY abstraction (Rust native)

- **portable-pty** (0.9): Cross-platform PTY creation. Same library used by WezTerm, pilotty, and agent-tui.
- **vt100** (0.16): VT100 terminal emulator. Parses ANSI escape sequences, maintains in-memory screen buffer.
- **tokio**: Async runtime for non-blocking PTY I/O.

```rust
use portable_pty::{PtySize, CommandBuilder, NativePtySystem};
use vt100::Parser;
use std::sync::{Arc, Mutex};

pub struct PtySession {
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    alive: Arc<Mutex<bool>>,
    cols: u16,
    rows: u16,
}

impl PtySession {
    fn new(command: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self>;
    fn with_cwd(command: &str, args: &[&str], cols: u16, rows: u16, cwd: &str) -> Result<Self>;
    fn inject_prompt(&self, text: &str) -> Result<()>;
    async fn wait_for_stable(&self, settle_duration: Duration, poll_interval: Duration) -> Result<String>;
    fn wait_for_stable_sync(&self, settle_duration: Duration, poll_interval: Duration) -> Result<String>;
    fn screen_text(&self) -> String;
    fn screen_hash(&self) -> u64;  // FNV-1a for change detection
    fn is_alive(&self) -> bool;
    fn kill(&self) -> Result<()>;
}
```

---

## MCP Server

MCP uses JSON-RPC 2.0 over HTTP+SSE (Streamable HTTP). Implemented with `rmcp`.

### Exposed tool

```json
{
  "name": "run_agent",
  "description": "Run a prompt through the Antigravity CLI agent.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "prompt": {
        "type": "string",
        "description": "The prompt to send to the agent"
      },
      "session_id": {
        "type": "string",
        "description": "Optional session ID for multi-turn. Omit for single-turn."
      }
    },
    "required": ["prompt"]
  }
}
```

### Endpoint

`http://<host>:<port>/mcp`

---

## ACP Server

ACP uses REST APIs. Implemented with `axum`.

### Endpoints

```
GET  /agents          → List available agents
POST /runs            → Create a new run
GET  /runs/:id        → Get run status/result
DELETE /runs/:id      → Close run and its session
```

### Agent discovery

```json
// GET /agents
{
  "agents": [
    {
      "name": "antigravity",
      "description": "Google Antigravity CLI agent (gemini-2.5-pro)",
      "metadata": {
        "model": "gemini-2.5-pro",
        "session_mode": "single"
      }
    }
  ]
}
```

### Run creation

```json
// POST /runs
// Request (single-turn):
{
  "agent_name": "antigravity",
  "input": [
    {
      "role": "user",
      "parts": [
        { "type": "text", "text": "Refactor the auth module to use JWT" }
      ]
    }
  ]
}

// Request (multi-turn with session_id):
{
  "agent_name": "antigravity",
  "session_id": "sess_abc123",
  "input": [
    {
      "role": "user",
      "parts": [
        { "type": "text", "text": "Refactor the auth module to use JWT" }
      ]
    }
  ]
}

// Response (sync):
{
  "run_id": "run_abc123",
  "session_id": null,
  "status": "completed",
  "output": [
    {
      "role": "agent",
      "parts": [
        { "type": "text", "text": "I've refactored the auth module..." }
      ]
    }
  ]
}

// Response (sync, multi-turn):
{
  "run_id": "run_def456",
  "session_id": "sess_abc123",
  "status": "completed",
  "output": [
    {
      "role": "agent",
      "parts": [
        { "type": "text", "text": "I've refactored the auth module..." }
      ]
    }
  ]
}

// Response (streaming via Accept: text/event-stream):
event: started
data: {"run_id":"run_abc123","status":"running"}

event: output
data: {"parts":[{"type":"text","text":"Analyzing the auth module..."}]}

event: completed
data: {"run_id":"run_abc123","status":"completed"}
```

---

## Session Management

### Single-turn (stateless)

Each request spawns a new `agy -p` process. No state between calls.

### Multi-turn (stateful)

Sessions are identified by `session_id`. PTY sessions are stored directly in the `AntigravityDriver`:

```rust
// Inside AntigravityDriver
sessions: Arc<Mutex<HashMap<String, PtySession>>>
```

- **Create**: First request with a `session_id` spawns an interactive `agy` process inside a PTY (via `spawn_blocking`).
- **Reuse**: Subsequent requests inject prompts into the existing PTY session.
- **Response extraction**: Screen diff (before/after injection) combined with TUI chrome stripping to extract clean agent output.
- **Cleanup**: `DELETE /runs/:id` endpoint closes the run and its associated session, or sessions are auto-recreated when the underlying PTY process dies.

---

## Docker Image

### Dockerfile

```dockerfile
FROM ubuntu:24.04

# System deps
RUN apt-get update && apt-get install -y \
    ca-certificates curl git jq \
    && rm -rf /var/lib/apt/lists/*

# Node.js (for MCP servers that agents may use)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs

# Antigravity CLI
RUN curl -fsSL https://antigravity.google/install.sh | bash

# Config directory
RUN mkdir -p /root/.gemini/antigravity-cli

# Wrapper binary
COPY agentbox-wrapper /usr/local/bin/agentbox-wrapper

WORKDIR /workspace
EXPOSE 7080

ENTRYPOINT ["agentbox-wrapper"]
```

### Demo docker-compose.yml

Demonstrates concurrent agy instances with different models — the core pain point this project solves:

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

---

## Demo

No custom client code. Use existing tools.

### ACP (via curl)

```bash
# Discover agents
curl http://localhost:9091/agents

# Run a prompt (single-turn)
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"explain this project"}]}]
  }'

# Run a prompt (multi-turn, first message creates session)
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "session_id": "sess_abc123",
    "input": [{"role":"user","parts":[{"type":"text","text":"explain this project"}]}]
  }'

# Follow-up in the same session
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "session_id": "sess_abc123",
    "input": [{"role":"user","parts":[{"type":"text","text":"now refactor the main module"}]}]
  }'

# Close a run and its session
curl -X DELETE http://localhost:9091/runs/run_abc123
```

### MCP (via any MCP client)

In QoderWork, Claude Code, Cursor, or any MCP-compatible client, add:

```
Server URL: http://localhost:9091/mcp
```

Then call the `run_agent` tool:

```
run_agent(prompt: "explain this project")
```

---

## Implementation Phases

### Phase 1: Skeleton (MVP — single-turn agy)

- [x] Cargo workspace with agentbox-wrapper crate
- [x] Config injector: read env vars → generate agy settings.json
- [x] HTTP server (axum) with health endpoint
- [x] agy driver: single-turn mode (spawn `agy -p`, capture output)
- [x] ACP endpoints: `GET /agents`, `POST /runs` (sync)
- [x] MCP server: `run_agent` tool (rmcp)
- [x] Dockerfile + docker-compose.yml
- [x] README with demo instructions

### Phase 2: Multi-turn support

- [x] PTY session manager (portable-pty + vt100)
- [x] agy driver: interactive mode with prompt injection
- [x] Screen stability detection for response completion
- [x] Session lifecycle (create, reuse, timeout, cleanup)
- [x] ACP streaming responses (SSE)

### Phase 3: Polish

- [ ] MCP stdio transport (HTTP/SSE only for now)
- [x] Logging and observability (tracing)
- [ ] Error recovery (basic handling in place, needs retry/backoff)
- [x] MCP server injection into agy config

### Phase 4: More agents

- [ ] Claude Code driver (`--output-format json`, `--bare`)
- [ ] Codex CLI driver
- [ ] Generic driver (stdin/stdout)

---

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `tokio` (1, full) | Async runtime |
| `axum` (0.8) | HTTP server (ACP REST + MCP HTTP) |
| `rmcp` | MCP server SDK |
| `portable-pty` (0.9) | PTY management (multi-turn) |
| `vt100` (0.16) | VT100 terminal emulation (multi-turn) |
| `serde` / `serde_json` | Serialization |
| `schemars` (1.0) | MCP tool schema generation |
| `dirs` (6) | Home directory resolution |
| `async-trait` (0.1) | Async trait support for drivers |
| `tracing` / `tracing-subscriber` | Structured logging |
| `uuid` | Session/run ID generation |
| `tower-http` | CORS middleware |
