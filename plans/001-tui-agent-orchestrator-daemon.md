# T-ACP MVP Plan: Foreground TUI Wrapper with Background Control Service

## Summary

Build a Linux-first local control system where users launch supported agents through `t-acp`, for example `t-acp opencode`. In this model, `t-acp` directly wraps the agent's interactive TTY session so the user can keep using keyboard and mouse normally, while a background service tracks all launched sessions and exposes high-level RPC control over concrete running agent instances via HTTP and WebSocket.

The MVP focuses on `opencode` first, but the architecture must make future support for `codex`, `claude-code`, and other agents straightforward through a stable internal adapter interface. The control model should prioritize stable, deterministic terminal interactions instead of human-like clicking. Default control paths should use keyboard navigation, paste, known shortcuts, and focus movement; screen-location mouse simulation can be added later as an enhancement. UI understanding should use a hybrid approach: maintain a full virtual terminal buffer and also surface incremental signals from the output stream for faster state transitions.

## Key Changes

### Runtime Model

- Implement the system as two cooperating runtimes:
  - `t-acp CLI wrapper` launches the target agent in the foreground, owns the user's active terminal session, and passes through user input and terminal events.
  - `t-acp background service` tracks all active sessions, maintains parsed terminal state, serializes remote actions, and serves RPC/event APIs.
- When the user runs `t-acp <agent> [agent-args...]`, `t-acp` should:
  - ensure the background service is running, starting it automatically if needed
  - create and register a new tracked session
  - launch the target agent attached to a PTY controlled by `t-acp`
  - attach the foreground wrapper to that session so the user remains directly interactive
- Track session lifecycle states at minimum as `starting`, `ready`, `busy`, `blocked(permission)`, `errored`, and `exited`.
- Keep each session isolated with its own PTY, child process, parsed screen state, adapter instance, serialized action queue, and foreground attachment metadata.

### Core Subsystems

- `launch wrapper`
  - command dispatch for `t-acp opencode`, `t-acp codex`, `t-acp claude-code`
  - daemon bootstrap and session registration
  - foreground attach and detach lifecycle
- `session manager`
  - multi-session registry in the background service
  - session identity, process identity, TTY identity, and cleanup
  - coordination between attached local users and remote RPC callers
- `terminal runtime`
  - PTY creation, resize, input/output multiplexing, and signal handling
  - virtual terminal parsing and screen snapshot maintenance
  - passthrough of local keyboard and mouse input with minimal extra latency
- `adapter layer`
  - agent-specific launch, parsing, UI recognition, and action execution
- `control API`
  - instance-centric HTTP RPC
  - WebSocket for session state, screen diff, and action result events

### Public Interfaces and Types

- Define `AgentKind` with at least `opencode`, `codex`, and `claude_code`.
- Define `AgentInstanceId` as the primary RPC addressable identifier.
- Define a CLI entry shape:
  - `t-acp <agent> [agent-args...]`
- Define `SessionSpec` with at least:
  - `agent_kind`
  - `cwd`
  - `env_overrides`
  - `initial_size` (`cols`, `rows`)
  - `launch_args`
- Define `SessionState` to include:
  - lifecycle status
  - current screen summary
  - blocking reason, if any
  - inferred UI mode such as `normal`, `input`, `model_picker`, or `permission_prompt`
  - foreground attachment status
  - process and TTY identity for correlation and cleanup
- Keep `session` as the internal runtime unit, but expose only agent instances at the public API layer.
- Expose HTTP endpoints with instance-first routing:
  - `GET /agents`
  - `GET /agents/:instance_id`
  - `POST /agents/:instance_id/input`
  - `POST /agents/:instance_id/actions/approve-permission`
  - `POST /agents/:instance_id/actions/reject-permission`
  - `POST /agents/:instance_id/actions/switch-model`
  - `POST /agents/:instance_id/actions/send-prompt`
  - `POST /agents/:instance_id/resize`
  - `DELETE /agents/:instance_id`
- Expose WebSocket events:
  - `instance_started`
  - `instance_state_changed`
  - `screen_updated`
  - `permission_detected`
  - `action_result`
  - `instance_exited`

### Terminal Parsing and Action Execution

- Preserve direct local interactivity:
  - user keyboard input passes through to the wrapped agent
  - mouse events and terminal resize events also pass through
  - terminal output is mirrored into the parsed session state used by RPC controllers
- Maintain a virtual terminal screen buffer instead of only storing raw output text.
- Retain recent snapshots or diffs to support state recognition and debugging.
- Support the common ANSI/VT control sequences needed by modern TUI agents.
- Let each adapter perform structured recognition over the parsed screen state, including:
  - permission prompt detection
  - input area focus detection
  - model picker open or closed detection
  - current model text detection
- Use incremental output parsing only as an accelerator for key state transitions, not as the primary source of truth.
- Implement high-level actions with post-condition checks:
  - `send_prompt`: prefer paste plus submit
  - `approve_permission`: prefer shortcuts or deterministic focus navigation
  - `switch_model`: adapter-defined sequence plus success verification
- Define input arbitration between the local foreground user and remote RPC control:
  - only one injected remote action sequence may run at a time per session
  - remote actions execute through the same session input queue used by the wrapper
  - the MVP should favor deterministic remote action execution over unrestricted concurrent writes
- Return structured failures instead of assuming success:
  - `unsupported_action`
  - `ui_not_detected`
  - `ambiguous_ui_state`
  - `action_timeout`
  - `process_exited`
  - `pty_io_error`

### Adapter Model and Delivery Order

- Use a stable internal Rust adapter trait for extensibility; do not implement dynamic external plugins in the MVP.
- Implement the first fully working adapter for `opencode`.
- Register placeholders or scaffolding for `codex` and `claude-code` so later adapters fit the same contract.
- Deliver in this order:
  1. CLI entrypoint plus daemon bootstrap path for `t-acp <agent>`.
  2. PTY and foreground attach integration that can launch `opencode`, pass through local input, and capture parsed screen state.
  3. Background session registry and correlation between attached foreground processes and tracked sessions.
  4. Stable adapter trait and working `opencode` adapter.
  5. High-level RPC actions wired to adapter execution with input arbitration.
  6. Screen diff streaming, state machine refinement, and debug logging.
  7. Placeholder registration points for future adapters.

## Test Plan

- Foreground launch behavior:
  - `t-acp opencode` launches the wrapped agent and keeps the user directly interactive
  - local keyboard, mouse, and resize events reach the agent correctly
  - the background service auto-starts if absent and registers the session exactly once
- PTY lifecycle:
  - create session, launch child process, stream output, exit cleanly, and release resources
- Virtual terminal parsing:
  - validate cursor moves, line wraps, clears, partial redraws, and final screen snapshots
- Session state machine:
  - verify expected transitions across `starting`, `ready`, `busy`, `blocked`, and `exited`
- HTTP and WebSocket API:
  - list agent instances
  - target a specific instance directly
  - send input and invoke actions through instance-centric routes
  - subscribe to instance state events
- `opencode` adapter behavior:
  - detect permission prompts
  - approve permission and return to interactive state
  - send multi-line prompts reliably
  - switch model successfully and fail clearly when UI does not match
- Multi-session integration:
  - multiple separately launched foreground agents are all tracked by the same background service
  - sessions run concurrently without PTY or state cross-talk
  - one blocked session does not prevent controlling another
  - remote RPC control still works while a human is actively interacting with the same foreground session
  - agent crashes propagate to state and event streams correctly

## Assumptions

- MVP is Linux-first and does not promise macOS support initially.
- The product shape is a foreground CLI wrapper plus background service, not a standalone full-screen manager TUI in the first release.
- Public control semantics are instance-centric high-level actions first; raw terminal events remain an internal capability unless needed later.
- `opencode` is the first production adapter; `codex` and `claude-code` are extension targets, not MVP commitments.
- Dynamic plugin loading is intentionally deferred; a stable internal adapter contract is sufficient for the first version.
- Screen-coordinate mouse clicking is not required for MVP success unless a future target agent proves impossible to control through stable keyboard-driven flows.
