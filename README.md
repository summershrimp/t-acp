# t-acp

[中文](./README.zh.md)

`t-acp` is a local control layer for terminal-based TUI agents.

It lets you keep using interactive agents like `opencode`, `claude-code`, and `codex` directly in your current terminal, while also exposing a local HTTP API for scripts, automation, or other local processes to inspect status, read recent screen contents, send input, and trigger adapter actions.

## Overview

`t-acp` is useful when:

- You want to keep the native TUI experience instead of converting an agent into a pure API workflow.
- You want another local process to observe whether the agent is busy, blocked on a permission prompt, or running on a specific model.
- You want a simple local interface to send prompts, approve permission dialogs, cycle models, or terminate an instance.

Its goal is not remote session hosting. It combines an interactive foreground terminal session with a programmable local control surface on the same machine.

## What It Does

- Runs the agent in the foreground and preserves the original PTY/TUI interaction model.
- Ensures the local daemon is available and registers new instances automatically.
- Exposes a local HTTP API to list instances, inspect details, send input, run actions, and terminate instances.
- Maintains a recent terminal screen snapshot in `screen_tail` for external observation.
- Exposes runtime metadata such as current agent, model, provider, reasoning effort, context usage, and terminal focus state.
- Provides specialized adapter behavior for `opencode`, including permission prompt detection, prompt injection, and model cycling shortcuts.
- Falls back to a generic adapter for `claude-code`, `codex`, and unknown commands.

## Current Support

### `opencode`

- Detects permission prompts
- Uses bracketed paste for `send-prompt` and submits automatically
- Supports `approve-permission` / `reject-permission`
- Supports `previous-model` / `next-model`
- Attempts to extract runtime metadata: agent, model, provider, reasoning effort, and context usage

### Generic adapter

The following currently use the `generic` adapter:

- `claude-code`
- `codex`
- Any command without a dedicated adapter

The generic adapter supports:

- Instance registration
- Raw input injection
- `send-prompt`

The generic adapter does not currently provide reliable permission prompt detection or model switching behavior.

## Requirements

- Rust 2024 edition
- Cargo
- A terminal agent executable available on the local machine, such as `opencode`

Default listen address: `127.0.0.1:48974`

You can override it with an environment variable:

```bash
export T_ACP_ADDR=127.0.0.1:49001
```

Use `RUST_LOG` to control logging, for example:

```bash
RUST_LOG=info cargo run -- daemon
```

## Build

```bash
cargo build
```

## Quick Start

### 1. Start the daemon

Start it manually:

```bash
cargo run -- daemon
```

With an explicit address:

```bash
cargo run -- daemon --addr 127.0.0.1:49001
```

In many cases you do not need to start it manually. When you launch an agent through the wrapper, `t-acp` checks the daemon first and starts it in the background if needed.

### 2. Start an agent through the wrapper

```bash
cargo run -- opencode
```

Pass additional arguments through:

```bash
cargo run -- opencode --model gpt-5
```

You can also wrap other commands:

```bash
cargo run -- claude-code
cargo run -- codex
cargo run -- /path/to/custom-agent
```

When you run `t-acp <agent> ...`:

1. The wrapper checks whether the daemon is healthy.
2. If the daemon is not running, it starts one in the background.
3. The agent runs in a PTY in the foreground.
4. Screen output remains visible in your terminal and is also forwarded to the daemon.
5. The daemon records instance metadata, status, recent screen contents, and any runtime metadata it can infer.

## Common Usage

Start an agent:

```bash
cargo run -- opencode
```

List instances:

```bash
curl http://127.0.0.1:48974/agents
```

Send raw input to an instance:

```bash
curl -X POST \
  --data-binary $'hello from api\n' \
  http://127.0.0.1:48974/agents/<instance_id>/input
```

Send a prompt through the adapter:

```bash
curl -X POST \
  --data-binary 'Summarize the current repo structure.' \
  http://127.0.0.1:48974/agents/<instance_id>/actions/send-prompt
```

Approve an `opencode` permission prompt:

```bash
curl -X POST \
  http://127.0.0.1:48974/agents/<instance_id>/actions/approve-permission
```

Cycle to the next model:

```bash
curl -X POST \
  http://127.0.0.1:48974/agents/<instance_id>/actions/next-model
```

Terminate an instance:

```bash
curl -X DELETE \
  http://127.0.0.1:48974/agents/<instance_id>
```

## Public API

All endpoints listen on `http://127.0.0.1:48974` by default.

### Read endpoints

#### `GET /health`

Health check.

Example response:

```json
{
  "ok": true
}
```

#### `GET /agents`

Lists registered instances.

Example response:

```json
{
  "agents": []
}
```

#### `GET /agents/{instance_id}`

Returns details for a single instance.

### Write endpoints

Every public write endpoint except `GET /health`, `GET /agents`, and `GET /agents/{instance_id}` returns `202 Accepted` on success:

```json
{
  "queued": true,
  "adapter": "opencode"
}
```

Notes:

- `queued: true` means the command has been queued or sent through the internal runtime channel
- `adapter` indicates which adapter produced the action
- Raw `input` and `DELETE /agents/{instance_id}` are not adapter-generated actions, so they return `"adapter": null`

#### `POST /agents/{instance_id}/input`

Injects raw bytes into the instance PTY. The request body is written directly and does not need to be JSON.

Useful for:

- Regular text
- `\n` / `\r`
- Control characters, for example `Ctrl+C` as `0x03`

#### `POST /agents/{instance_id}/actions/send-prompt`

Sends a prompt action.

- For `opencode`: wraps the body in bracketed paste and submits it automatically
- For the generic adapter: appends a newline if the body does not already end with one

#### `POST /agents/{instance_id}/actions/approve-permission`

Approves a permission request.

- For `opencode`: only succeeds when a permission prompt is currently visible
- The current implementation sends Enter

#### `POST /agents/{instance_id}/actions/reject-permission`

Rejects a permission request.

- For `opencode`: only succeeds when a permission prompt is currently visible
- The current implementation sends `Esc`

#### `POST /agents/{instance_id}/actions/previous-model`

Cycles to the previous model.

- For `opencode`: sends `Shift+F2`
- The generic adapter currently returns `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/next-model`

Cycles to the next model.

- For `opencode`: sends `F2`
- The generic adapter currently returns `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/switch-model`

Reserved endpoint. The request body may eventually carry a target model identifier.

- `opencode` currently returns `501 unsupported_action`
- The generic adapter is also not implemented yet

#### `DELETE /agents/{instance_id}`

Sends `Ctrl+C` to request termination of the foreground agent.

## Instance Object

The objects returned by `GET /agents` and `GET /agents/{instance_id}` look like this:

```json
{
  "id": "opencode-12345-1716620000000",
  "agent_kind": "opencode",
  "adapter": "opencode",
  "pid": 12345,
  "cwd": "/path/to/project",
  "command": "opencode --model gpt-5",
  "status": "ready",
  "ui_mode": "input",
  "blocking_reason": null,
  "current_agent": "Build",
  "current_model": "GPT-5.4",
  "current_provider": "GitHub Copilot",
  "current_reasoning_effort": "high",
  "current_context_window": "42.6K",
  "current_context_usage_percent": 21,
  "focused": true,
  "exit_status": null,
  "created_at_ms": 1716620000000,
  "updated_at_ms": 1716620001234,
  "screen_tail": "...recent terminal screen contents..."
}
```

Key fields:

- `agent_kind`: normalized instance type such as `opencode`, `claude_code`, or `codex`
- `adapter`: the adapter actually in use; today only `opencode` has a dedicated adapter, while most others return `generic`
- `status`: `starting`, `ready`, `busy`, `blocked`, `exited`
- `ui_mode`: `unknown`, `normal`, `input`, `permission_prompt`, `model_picker`
- `blocking_reason`: currently only `permission` when a permission block is detected
- `current_agent`: agent name parsed from the runtime footer, such as `Build`
- `current_model`: current model name
- `current_provider`: current provider name, such as `GitHub Copilot`
- `current_reasoning_effort`: current reasoning level, such as `high`
- `current_context_window`: current context size, such as `42.6K`
- `current_context_usage_percent`: current context usage percentage, such as `21`
- `focused`: whether the outer terminal is currently focused
- `screen_tail`: recent terminal screen text maintained by the daemon for observation and adapter heuristics

## Focus State

`focused` depends on terminal focus reporting support.

The wrapper tries to enable it automatically. If the terminal or multiplexer does not support it, `focused` may remain at its default value.

If you are running inside `tmux`, enable:

```tmux
set -g focus-events on
```

## Error Semantics

Common errors include:

- `404 not_found`: the instance does not exist
- `409 process_exited`: the instance has already exited and can no longer accept actions
- `409 ui_not_detected`: the adapter requires a UI state that is not currently visible, for example no permission prompt is present
- `400 bad_request`: the request body is invalid, such as an empty prompt
- `501 unsupported_action`: the current adapter does not support that action yet

## Current Limitations

- The daemon registry is in-memory only, so instance data is not persisted across daemon restarts
- `screen_tail` is based on terminal screen contents, not a complete output log
- `switch-model` is not implemented yet
- A remote `resize` API is not wired yet
- `focused` depends on proper focus event forwarding from the terminal and any multiplexer such as `tmux`
- Adapter state detection is heuristic; `opencode` UI detection can still misclassify some screens
- The service listens on loopback by default and has no authentication; do not expose it directly to the public internet

## Development And Testing

Formatting:

```bash
cargo fmt
cargo fmt --check
```

Run tests:

```bash
cargo test
```

Manual smoke test:

```bash
target/debug/t-acp daemon --addr 127.0.0.1:49001
T_ACP_ADDR=127.0.0.1:49001 target/debug/t-acp /bin/cat
```

Then send input from another terminal:

```bash
curl -X POST \
  --data-binary $'ping\n' \
  http://127.0.0.1:49001/agents/<instance_id>/input
```

## Project Structure

```text
src/main.rs              CLI entry point
src/wrapper.rs           foreground wrapper, daemon bootstrap, PTY and RPC forwarding
src/daemon.rs            local HTTP control service and instance registry
src/adapters/            adapter trait and implementations
src/adapters/generic.rs  generic adapter
src/adapters/opencode.rs opencode adapter and metadata extraction
src/pty.rs               Unix PTY spawn and resize support
src/http.rs              daemon client
src/api.rs               HTTP request and response structures
src/internal.rs          internal WebSocket messages between wrapper and daemon
src/util.rs              small utility helpers
plans/                   design and implementation notes
```

## Internal Runtime Notes

The following endpoints are primarily for internal wrapper-daemon communication and are not intended for external callers:

- `POST /internal/agents/register`
- `GET /internal/agents/{instance_id}/ws` for WebSocket upgrade
- `POST /internal/agents/{instance_id}/exit`

The wrapper and daemon currently use `/internal/agents/{instance_id}/ws` to carry:

- PTY output from the wrapper to the daemon
- Resize and focus runtime events from the wrapper to the daemon
- Queued commands from the daemon back to the wrapper

In other words, the runtime data plane now goes through the internal WebSocket. Internal HTTP is mainly used for registration and exit reporting.
