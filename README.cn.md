# agentbox

[English](README.md)

**CLI AI Agent 的容器化运行时。**

将 Antigravity CLI (agy)、Claude Code、Codex、GitHub Copilot 等命令行 AI Agent 封装在 Docker 容器中，通过 MCP 和 ACP 协议对外暴露，供其他 Agent 调用。

## 为什么需要 agentbox？

以 Antigravity CLI 为例，它有一个关键痛点：**无法在非交互模式下指定模型**。

`agy -p "prompt"` 是单次的，`-c`/`--continue` 等会话参数只在交互 TUI 模式下生效，而模型选择必须修改 `settings.json`。当多个 agy 实例需要不同模型并发运行时，它们会争抢同一个配置文件。

容器化解决了所有这些问题：每个容器拥有独立的文件系统、独立的 `settings.json`、独立的 agy 进程。

## 快速开始

### 前置条件

- Docker 和 docker-compose
- Rust 工具链（用于本地开发）

### 构建

```bash
# 编译 wrapper 二进制
cargo build --release

# 构建 Docker 镜像
cp target/release/agentbox-wrapper docker/
docker build -t agentbox/antigravity -f docker/Dockerfile docker/
```

### 运行 Demo

使用 `docker-compose.yml` 同时启动两个不同模型的 agy 实例：

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

### 调用方式

**ACP（HTTP REST）：**

```bash
# 发现可用 Agent
curl http://localhost:9091/agents

# 单次运行
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"解释这个项目"}]}]
  }'

# 多轮对话（首轮，获取 session_id）
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"分析 auth 模块"}]}],
    "session_id": "my-session-1"
  }'

# 多轮对话（后续，同一 session）
curl -X POST http://localhost:9091/runs \
  -H 'Content-Type: application/json' \
  -d '{
    "agent_name": "antigravity",
    "input": [{"role":"user","parts":[{"type":"text","text":"把它改成 JWT"}]}],
    "session_id": "my-session-1"
  }'

# 关闭会话
curl -X DELETE http://localhost:9091/runs/run_abc123
```

**MCP（JSON-RPC over HTTP）：**

在任何 MCP 客户端（QoderWork、Claude Code、Cursor 等）中添加：

```
Server URL: http://localhost:9091/mcp
```

然后调用 `run_agent` 工具：

```
run_agent(prompt: "解释这个项目")
run_agent(prompt: "继续优化", session_id: "my-session-1")
```

**Hermes Agent：**

[Hermes Agent](https://github.com/NousResearch/hermes-agent) 可以自动发现并调用 agentbox 容器的 MCP 工具：

```bash
# 添加 agentbox MCP 服务器
hermes mcp add agentbox-flash --url http://localhost:9091/mcp
hermes mcp add agentbox-pro   --url http://localhost:9092/mcp
hermes mcp add agentbox-opus  --url http://localhost:9093/mcp

# 列出已配置的服务器
hermes mcp list

# 移除服务器
hermes mcp remove agentbox-flash

# 测试连接
hermes mcp test agentbox-flash
```

每个容器的 `tools/list` 响应中包含模型名称（如 `AgentBox — Gemini 3.1 Pro (High)`），LLM 可根据描述选择合适的模型。

---

## 配置

所有配置通过环境变量注入，无需管理宿主机配置文件。

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `AGENTBOX_MODEL` | *（必填）* | 模型名称，如 `gemini-2.5-pro` |
| `AGENTBOX_AGENT` | `antigravity` | Agent 类型。通过[驱动工厂](#架构)自动选择对应实现。支持：`antigravity` |
| `AGENTBOX_PORT` | `7080` | HTTP 端口（MCP + ACP 共用） |
| `AGENTBOX_SESSION_MODE` | `single` | `single`（单次）或 `multi`（会话） |
| `AGENTBOX_YOLO` | `false` | 自动批准所有 Agent 工具调用 |
| `AGENTBOX_MCP_SERVERS` | *（空）* | 注入到 Agent 的 MCP 服务器配置（JSON） |
| `AGENTBOX_WORKSPACE` | `/workspace` | 容器内工作目录 |
| `AGENTBOX_SESSION_TIMEOUT` | `1800` | 多轮会话超时（秒） |

### 示例

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

## API 参考

### ACP 端点

```
GET    /agents          列出可用 Agent
POST   /runs            创建运行（支持可选 session_id 实现多轮）
GET    /runs/:id        获取运行状态/结果
DELETE /runs/:id        关闭运行及其关联会话
```

**POST /runs 请求：**

```json
{
  "agent_name": "antigravity",
  "input": [
    {
      "role": "user",
      "parts": [{"type": "text", "text": "你的 prompt"}]
    }
  ],
  "session_id": "可选，多轮对话使用"
}
```

**POST /runs 响应：**

```json
{
  "run_id": "run_abc123",
  "session_id": "sess_xyz789",
  "status": "completed",
  "output": [
    {
      "role": "agent",
      "parts": [{"type": "text", "text": "Agent 的回复..."}]
    }
  ]
}
```

### MCP 工具

```json
{
  "name": "run_agent",
  "inputSchema": {
    "properties": {
      "prompt":     { "type": "string", "description": "发送给 Agent 的 prompt" },
      "session_id": { "type": "string", "description": "可选，多轮会话 ID" }
    },
    "required": ["prompt"]
  }
}
```

---

## 架构

```
┌─── Host ──────────────────────────────────────────────┐
│                                                        │
│  docker run / docker-compose                           │
│       │                                                │
│       ▼                                                │
│  ┌─── Container ────────────────────────────────────┐  │
│  │                                                   │  │
│  │  agentbox-wrapper（单一 Rust 二进制）               │  │
│  │                                                   │  │
│  │  ┌──────────────┐  ┌───────────────────────────┐ │  │
│  │  │ 配置注入器    │  │ 驱动工厂                  │ │  │
│  │  │              │  │                           │ │  │
│  │  │ env vars ──► │  │  AGENTBOX_AGENT ──►       │ │  │
│  │  │ settings.json│  │  antigravity 驱动         │ │  │
│  │  └──────────────┘  │                           │ │  │
│  │                    │  单轮: agy -p "..."      │ │  │
│  │                    │  多轮: PTY + VT100 交互    │ │  │
│  │                    └───────────────────────────┘ │  │
│  │                                                   │  │
│  │  ┌──────────────┐                                 │  │
│  │  │ MCP Server   │  (rmcp, JSON-RPC)               │  │
│  │  ├──────────────┤                                 │  │
│  │  │ ACP Server   │  (axum, REST)                   │  │
│  │  └──────────────┘                                 │  │
│  │                                                   │  │
│  │  /workspace  ←── 宿主机卷挂载                      │  │
│  └───────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────┘
```

### 技术栈

| 组件 | 技术 |
|------|------|
| 语言 | Rust（全栈） |
| 异步运行时 | tokio |
| HTTP 框架 | axum 0.8 |
| MCP SDK | rmcp 1.7 |
| PTY | portable-pty 0.9 |
| 终端模拟 | vt100 0.16 |
| 序列化 | serde / serde_json |
| 日志 | tracing |

### 项目结构

```
agentbox/
├── Cargo.toml                       # workspace 根
├── crates/
│   └── agentbox-wrapper/
│       └── src/
│           ├── main.rs              # 入口：读 env → 生成配置 → 启动服务器
│           ├── config.rs            # 环境变量解析
│           ├── mcp.rs               # MCP 服务器（rmcp）
│           ├── acp.rs               # ACP REST 端点（axum）
│           ├── session.rs           # PTY 会话管理（portable-pty + vt100）
│           ├── output_parser.rs     # ANSI 剥离 + TUI 界面元素过滤
│           └── drivers/
│               ├── mod.rs           # AgentDriver trait + 驱动工厂
│               └── antigravity.rs   # agy 驱动（单轮 + 多轮、配置初始化、TUI 清理）
├── docker/
│   ├── Dockerfile
│   └── docker-compose.yml
├── DESIGN.md                        # 详细设计文档
├── README.md                        # English version
└── README.cn.md                     # 本文件（中文）
```

---

## 开发

```bash
# 运行全部测试
cargo test

# Clippy 检查
cargo clippy

# Release 构建
cargo build --release

# 带日志运行（本地调试）
AGENTBOX_MODEL=gemini-2.5-pro RUST_LOG=debug cargo run
```

### 当前测试覆盖

80 个单元测试，覆盖：配置解析（13）、ACP 端点（11）、MCP 服务器（3）、Agent 驱动（6）、PTY 会话（7）、输出解析（40）。

---

## 路线图

- [x] **Phase 1** — 单轮 MVP：配置注入、agy -p 驱动、MCP + ACP 端点
- [x] **Phase 2** — 多轮支持：PTY 会话管理、屏幕稳定性检测、会话生命周期
- [x] **Phase 2.5** — ACP 流式响应（SSE）
- [ ] **Phase 3** — MCP stdio 传输、错误恢复增强
- [ ] **Phase 4** — 更多 Agent 驱动（Claude Code、Codex、GitHub Copilot）—— *驱动工厂架构已就位，添加新驱动只需实现 AgentDriver trait + 一行 match arm*

---

## License

MIT
