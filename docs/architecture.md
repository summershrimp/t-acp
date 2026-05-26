# t-acp Architecture

[中文](./architecture.zh.md)

This document describes the current `t-acp` architecture. The implementation is a non-invasive local wrapper for terminal agents: it observes and controls a real PTY session without requiring hooks, plugins, patches, provider API interception, or model-response parsing inside the wrapped CLI/TUI.

## Design Goals

- Keep the native foreground TUI experience.
- Make every wrapped instance observable and controllable through local APIs.
- Use terminal screen parsing as the portable source of truth for human-visible state.
- Route human-interaction prompts through stable structured requests and submissions.
- Keep agent-specific behavior isolated behind adapters.
- Support `opencode` first while keeping the adapter boundary usable for `claude-code`, `codex`, and other TUI agents later.

## Runtime Shape

`t-acp` has two cooperating runtimes:

```text
user terminal
    |
    v
t-acp wrapper
    |
    | owns PTY, mirrors output, forwards input/resize/focus
    v
wrapped agent process

t-acp wrapper <---- internal WebSocket ----> t-acp daemon
                                             |
                                             | HTTP/SSE/HTML
                                             v
                                      local controllers
```

The wrapper stays in the foreground and owns the target agent's PTY. The daemon runs in the background and owns shared observation/control state for all registered instances.

## Main Components

### CLI And Wrapper

Implemented mainly in `src/main.rs` and `src/wrapper.rs`.

Responsibilities:

- Dispatch `t-acp daemon` versus `t-acp <agent> [args...]`.
- Ensure the daemon is running before launching a wrapped agent.
- Preserve the caller's current working directory for the wrapped command.
- Spawn the target command in a PTY.
- Pass local keyboard and terminal input to the PTY.
- Mirror PTY output to the user's terminal and to the daemon.
- Forward local terminal resize and focus events to the daemon.
- Receive queued remote commands from the daemon and write them to the PTY.

### PTY Runtime

Implemented mainly in `src/pty.rs`.

Responsibilities:

- Create a Unix PTY.
- Spawn the child command inside the requested working directory.
- Resize the PTY from wrapper-local `SIGWINCH` events.
- Provide reader/writer handles used by the wrapper.

### Daemon

Implemented mainly in `src/daemon.rs`.

Responsibilities:

- Maintain the in-memory registry of active instances.
- Store instance metadata, lifecycle state, terminal focus, and exit status.
- Maintain a vt100 screen snapshot for each instance.
- Keep raw PTY and observation event ring buffers for debugging.
- Run adapter recognition after screen updates.
- Expose public HTTP, SSE, and HTML observation endpoints.
- Queue input/action bytes for delivery back to the wrapper.

The registry is in-memory only. Restarting the daemon loses tracked instance metadata and observation buffers.

### Internal Runtime Channel

Defined by `src/internal.rs` and bridged by `src/http.rs`/`src/daemon.rs`.

All wrapper-to-daemon runtime traffic uses:

```text
GET /internal/agents/{instance_id}/ws
```

The WebSocket carries:

- PTY output frames from wrapper to daemon
- terminal resize frames from wrapper to daemon
- focus frames from wrapper to daemon
- queued command frames from daemon to wrapper

Internal HTTP is only used for registration and exit reporting:

- `POST /internal/agents/register`
- `POST /internal/agents/{instance_id}/exit`

Do not add internal HTTP output, resize, or command-polling fallbacks unless a concrete compatibility issue requires it.

### Adapter Layer

Implemented in `src/adapters/`.

The adapter boundary is where agent-specific behavior belongs:

- recognize UI mode and blocking state from the parsed screen
- extract metadata such as model/provider/context where possible
- build high-level action byte sequences
- parse human-interaction prompts into structured `interaction_request` values
- submit structured interactions back through deterministic stdin sequences

Current adapters:

- `opencode`: dedicated adapter with structured permission/question parsing and model shortcuts.
- `generic`: fallback for `claude-code`, `codex`, and unknown commands. It supports registration, raw input, and simple prompt sending only.

Adapters may use existing CLI-native structured outputs as optional enrichment, but the base state source remains the PTY screen.

### API Types

Implemented mainly in `src/api.rs` and `src/interactions.rs`.

Key public structures:

- `AgentView`: public instance state returned by `/agents`.
- `ObservationView`: screen snapshot, event tail, and raw PTY tail for debugging.
- `InteractionRequest`: structured human-input request detected from the TUI screen.
- `InteractionOption`: selectable option for a detected interaction.
- `SubmitInteractionRequest`: user/API submission for the visible interaction.

Interaction ids must be semantic and redraw-stable. They should be derived from stable fields such as source, kind, title, subject, prompt, and options. They must not include raw frame text, cursor positions, spinner output, or other volatile terminal artifacts.

## Data Flow

### Observation Flow

```text
agent writes TUI output
    -> PTY reader in wrapper
    -> user's terminal
    -> internal WebSocket output frame
    -> daemon raw PTY ring
    -> vt100 parser
    -> screen snapshot
    -> adapter observation
    -> AgentView / ObservationView / events
```

This flow lets the user keep the native TUI while local controllers observe the same visible state through HTTP, SSE, and the observation page.

### Human Interaction Flow

```text
TUI renders permission/question prompt
    -> daemon updates vt100 screen
    -> adapter detects InteractionRequest
    -> /agents and /observations expose structured request
    -> /observe renders human controls
    -> user submits option or custom answer
    -> POST /agents/{id}/interaction
    -> daemon checks interaction_id is still visible
    -> adapter converts submission into stdin bytes
    -> daemon queues command
    -> wrapper writes command to PTY
```

The stale-id check prevents an old browser panel or automation client from approving a prompt that is no longer visible.

### Action Flow

High-level actions use the same queue as structured interactions:

```text
POST /agents/{id}/actions/send-prompt
POST /agents/{id}/actions/approve-permission
POST /agents/{id}/actions/reject-permission
POST /agents/{id}/actions/previous-model
POST /agents/{id}/actions/next-model
```

The daemon asks the current adapter to build a deterministic input sequence. If the adapter cannot safely perform the action from the current UI state, it returns a structured error such as `ui_not_detected` or `unsupported_action`.

## Public Surfaces

Default address:

```text
http://127.0.0.1:48974
```

Read surfaces:

- `GET /health`
- `GET /agents`
- `GET /agents/{instance_id}`
- `GET /observe`
- `GET /agents/{instance_id}/observe`
- `GET /agents/{instance_id}/observations`
- `GET /agents/events/stream`
- `GET /agents/{instance_id}/events/stream`

Write surfaces:

- `POST /agents/{instance_id}/input`
- `POST /agents/{instance_id}/interaction`
- `POST /agents/{instance_id}/actions/send-prompt`
- `POST /agents/{instance_id}/actions/approve-permission`
- `POST /agents/{instance_id}/actions/reject-permission`
- `POST /agents/{instance_id}/actions/previous-model`
- `POST /agents/{instance_id}/actions/next-model`
- `POST /agents/{instance_id}/actions/switch-model`
- `DELETE /agents/{instance_id}`

`/observe` is a zero-build HTML observation plane embedded from `assets/observe.html`. Rebuild and restart the daemon after editing that template.

## State Model

Important `AgentView` fields:

- `status`: `starting`, `ready`, `busy`, `blocked`, or `exited`
- `ui_mode`: `unknown`, `normal`, `input`, `permission_prompt`, or `model_picker`
- `blocking_reason`: currently most often `permission`
- `need_interactive`: whether a human should intervene
- `interactive_kind`: high-level kind such as `permission` or `question`
- `interaction_request`: structured request when one is visible
- `focused`: whether the outer terminal reports focus
- `screen_tail`: recent parsed screen text

For debugging, `ObservationView` adds full screen lines, cursor position, recent observation events, and raw PTY tail data.

## Extension Guide

To add a dedicated adapter for another CLI/TUI:

1. Keep the wrapped CLI unchanged. Do not require hooks or plugins.
2. Add adapter-specific screen recognition in `src/adapters/`.
3. Parse visible blocking prompts into `InteractionRequest`.
4. Use semantic, redraw-stable interaction ids.
5. Implement submissions through deterministic stdin bytes.
6. Return structured errors when the expected UI state is not visible.
7. Add unit tests with representative screen fixtures.
8. Keep optional CLI-native metadata as enrichment, not as the base control path.

## Current Limitations

- The daemon registry and observation buffers are not persisted.
- Screen parsing is heuristic and can miss UI variants.
- `opencode` is the only dedicated adapter with structured interaction support today.
- `switch-model` is exposed but not implemented.
- There is no public remote resize API; wrapper-local resize is synchronized internally.
- The service has no authentication and should stay bound to loopback unless that changes.
