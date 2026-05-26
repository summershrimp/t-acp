# Repository Guidelines

## Project Structure & Module Organization

This is a Rust CLI project for wrapping TUI agents and exposing a local control API.

- `src/main.rs` handles CLI dispatch and process exit behavior.
- `src/wrapper.rs` owns foreground agent wrapping, detached daemon bootstrap, PTY forwarding, resize forwarding, and instance registration.
- `src/daemon.rs` implements the local HTTP control service, in-memory agent instance registry, vt100 screen maintenance, and command queue.
- `src/adapters/` contains agent-specific behavior; `opencode` is the first implemented adapter.
- `src/interactions.rs` contains stable interaction ids, evidence helpers, and submission validation.
- `src/pty.rs` contains Unix PTY process spawning, resize, and waiting.
- `src/http.rs` contains the blocking daemon client plus the internal WebSocket bridge; `src/util.rs` provides small helpers.
- `assets/observe.html` is the zero-build observation plane template embedded into the daemon binary.
- `docs/architecture.md` is the current architecture guide; prefer it over older plan notes when checking runtime boundaries.
- `plans/` stores design and implementation plans.

Unit tests currently live beside implementation code in `#[cfg(test)]` modules.

## Build, Test, and Development Commands

- `cargo build`: compile the project in debug mode.
- `cargo test`: run all unit tests.
- `cargo fmt`: format Rust source files.
- `cargo fmt --check`: verify formatting without changing files.
- `target/debug/t-acp daemon --addr 127.0.0.1:48974`: run the local control service.
- `T_ACP_ADDR=127.0.0.1:48974 target/debug/t-acp opencode`: run `opencode` through the wrapper.
- `T_ACP_ADDR=127.0.0.1:49087 target/debug/t-acp /bin/pwd`: quick wrapper/daemon smoke test on a temporary port.

Use a non-default `T_ACP_ADDR` when running smoke tests to avoid interfering with an existing daemon.

Cargo network access can be unreliable in this environment. If adding or installing third-party crates fails with a network error, retry the Cargo command several times instead of replacing the dependency with a hand-written implementation.

## Coding Style & Naming Conventions

Use standard Rust formatting via `rustfmt`; keep code `cargo fmt --check` clean. Prefer small modules with explicit ownership boundaries. Use `snake_case` for functions, variables, files, and JSON fields; use `PascalCase` for Rust types and enums.

Keep public behavior instance-centric in API naming: routes should target `/agents/:instance_id/...`, while `session` remains an internal runtime concept.

## Architecture Constraints

t-acp is a non-invasive wrapper. Do not require or implement hooks, plugins, patches, or other in-process extensions for wrapped CLI/TUI tools such as opencode, Claude Code, or Codex.

The required base layer is wrapper-owned PTY observation plus wrapper-owned stdin/action control. PTY screen parsing is the portable ground truth for what the user can see, and stdin control is the portable control plane for approve/reject/select/custom-answer interactions.

All wrapper-to-daemon internal runtime traffic must flow through the internal WebSocket channel at `/internal/agents/:instance_id/ws`. Do not add new internal HTTP endpoints for output, resize, command delivery, or similar runtime events unless there is a very specific compatibility need.

CLI-native structured outputs may be used only as optional adapter enrichment when they already exist without modifying the wrapped tool, for example documented local APIs, JSON logs, debug streams, or trace files. Do not make provider API response interception, MITM traffic analysis, or model-response parsing the primary state source for local TUI interaction state.

Interaction ids must be stable across redraw noise. Do not include raw screen text, spinner frames, cursor positions, or other volatile rendering artifacts in `interaction_request.id`; use semantic fields such as source, kind, title, subject, prompt, and options.

Keep daemon bootstrap resilient. A stale or poisoned in-memory registry must not make `/health` look healthy while `/internal/agents/register` panics; recover poisoned locks or return structured errors. Wrapper-started daemons should stay alive after a short-lived wrapper exits.

For terminal parsing changes, preserve terminal-size synchronization: wrapper registration and local `SIGWINCH` resize events update the daemon's vt100 screen through the internal WebSocket, and parser-side dynamic growth may be used to avoid truncating large-screen TUI prompts. There is no public remote resize endpoint in the current implementation.

## Testing Guidelines

Add unit tests near the code they cover. Name tests by behavior, for example `opencode_detects_permission_prompt` or `form_round_trip_handles_spaces_and_symbols`.

Run `cargo fmt`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo build`, and `git diff --check` before handing off changes. For daemon or PTY work, also run a smoke test with a temporary port and a harmless command such as `/bin/echo`, `/bin/pwd`, or `/bin/cat`.

For observation-plane changes, remember `assets/observe.html` is embedded via `include_str!`; rebuild and restart the daemon before validating the browser page.

## Commit & Pull Request Guidelines

Every completed task must end with a git commit.

Use commit titles in the `type: title` format. Keep `title` specific and descriptive enough to summarize the work completed in that commit.

Each commit message body must include:

- the user's original input from the current agent interaction
- a concise summary of the agent's analysis
- a concise summary of the implementation or development work completed

Pull requests should include a concise summary, notable behavior changes, and the exact verification commands run. For API changes, include example routes or payloads. For TUI/PTY changes, describe the manual smoke test and platform used.

## Security & Configuration Tips

The daemon listens on `127.0.0.1` by default. Do not expose it on public interfaces without adding authentication and input restrictions. Treat RPC input as equivalent to typing into the wrapped agent's terminal.
