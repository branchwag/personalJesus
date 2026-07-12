# pj — Coding Agent Guide

## Project Overview

pj is a local-first Rust AI chat app. It provides a web UI (Actix-web) and a TUI/CLI (Ratatui), both backed by Ollama for inference and SQLite for chat persistence. No cloud dependencies.

## Build & Run

```bash
# Build both binaries
cargo build --release

# Web UI (default binary)
cargo run --release --bin pj-web

# CLI TUI or one-shot
cargo run --release --bin pj
cargo run --release --bin pj -- "your question here"

# Lint / typecheck (Rust — cargo check covers this)
cargo check
```

There are no unit tests. `cargo check` is the verification step.

## Architecture

```
src/
  main.rs   — pj-web binary: Actix-web server, REST API, serves static UI
  cli.rs    — pj binary: Ratatui TUI + one-shot CLI mode
  lib.rs    — Shared: DB (SQLite via r2d2), Ollama API client, types
  tools.rs  — Tool definitions, execution, text-based tool call parsing
static/     — Web UI frontend (HTML/JS/CSS)
data/       — SQLite database (chat.db)
```

### Two binaries, one lib

- **pj-web** (`main.rs`): HTTP server on configurable PORT. Endpoints:
  - `POST /api/chat` — streaming generate (basic, no tools)
  - `POST /api/chat/tools` — chat with tool support (sessions)
  - `POST /api/chat/tools/confirm` — approve/deny tool execution
  - CRUD for chats/messages
- **pj** (`cli.rs`): TUI (interactive) or one-shot (pass question as args). Shares the same SQLite DB as the web UI.

### Ollama Integration

- Base URL: `OLLAMA_URL` env var (default `http://localhost:11434`)
- Model: `MODEL_NAME` env var (default `gemma2:9b`)
- Web UI basic chat uses `/api/generate` (simple, streaming, no tools)
- Web UI tool chat and CLI use `/api/chat` (non-streaming, with tools)
- `chat_with_ollama()` in `lib.rs` is the shared chat API client
- If the model doesn't support tools, `chat_with_ollama` auto-retries without them

### Tool System

Tools are defined in `tools.rs`. Available tools: `read_file`, `write_file`, `edit_file`, `run_command`, `glob`, `grep`, `read_directory`.

Two tool detection paths:
1. **Native**: Ollama returns `tool_calls` in the response JSON (for models that support it)
2. **Text fallback**: `parse_tool_calls_from_text()` extracts `<tool_call>` XML tags from response text

Tool execution requires user confirmation in both CLI (y/N prompt) and web UI (confirm endpoint).

### Database

SQLite via r2d2 connection pool. Schema:
- `chats` — id, title, created_at
- `messages` — id, chat_id, role, content, created_at

DB path: `DATABASE_URL` env var (default `data/chat.db`).

## Code Conventions

- No comments in code unless explicitly asked
- Error handling: propagate with `map_err` and `format!`, never `unwrap()` in production paths (DB pool init is the exception)
- Async: tokio runtime, `tokio::spawn` for background work
- Serialization: serde + serde_json, `#[serde(skip_serializing_if)]` for optional fields
- Environment config: `get_env_or(key, default)` pattern throughout
- Frontend: vanilla JS in `static/`, no build step

## Known Quirks

- Models that don't support tools (like gemma2) will error on `/api/chat` with tools — the auto-retry handles this
- `OllamaChatMessage.content` uses a custom deserializer to handle `null` from Ollama (models return null content when issuing tool calls)
- The TUI uses crossterm raw mode + alternate screen; panics in the TUI loop will leave the terminal in a broken state (run `reset` to fix)
- Both binaries share `lib.rs` — changes to Ollama types affect both
