# Repository Guidelines

## Project Structure & Module Organization

This is a Rust CLI project for wrapping TUI agents and exposing a local control API.

- `src/main.rs` handles CLI dispatch and process exit behavior.
- `src/wrapper.rs` owns foreground agent wrapping, daemon bootstrap, PTY forwarding, and instance registration.
- `src/daemon.rs` implements the local HTTP control service and in-memory agent instance registry.
- `src/adapters.rs` contains agent-specific behavior; `opencode` is the first implemented adapter.
- `src/pty.rs` contains Unix PTY process spawning and waiting.
- `src/http.rs` and `src/util.rs` provide small internal helpers.
- `plans/` stores design and implementation plans.

Unit tests currently live beside implementation code in `#[cfg(test)]` modules.

## Build, Test, and Development Commands

- `cargo build`: compile the project in debug mode.
- `cargo test`: run all unit tests.
- `cargo fmt`: format Rust source files.
- `cargo fmt --check`: verify formatting without changing files.
- `target/debug/t-acp daemon --addr 127.0.0.1:48974`: run the local control service.
- `T_ACP_ADDR=127.0.0.1:48974 target/debug/t-acp opencode`: run `opencode` through the wrapper.

Use a non-default `T_ACP_ADDR` when running smoke tests to avoid interfering with an existing daemon.

Cargo network access can be unreliable in this environment. If adding or installing third-party crates fails with a network error, retry the Cargo command several times instead of replacing the dependency with a hand-written implementation.

## Coding Style & Naming Conventions

Use standard Rust formatting via `rustfmt`; keep code `cargo fmt --check` clean. Prefer small modules with explicit ownership boundaries. Use `snake_case` for functions, variables, files, and JSON fields; use `PascalCase` for Rust types and enums.

Keep public behavior instance-centric in API naming: routes should target `/agents/:instance_id/...`, while `session` remains an internal runtime concept.

All wrapper-to-daemon internal runtime traffic must flow through the internal WebSocket channel at `/internal/agents/:instance_id/ws`. Do not add new internal HTTP endpoints for output, resize, command delivery, or similar runtime events unless there is a very specific compatibility need.

## Testing Guidelines

Add unit tests near the code they cover. Name tests by behavior, for example `opencode_detects_permission_prompt` or `form_round_trip_handles_spaces_and_symbols`.

Run `cargo test` before handing off changes. For daemon or PTY work, also run a smoke test with a temporary port and a harmless command such as `/bin/echo` or `/bin/cat`.

## Commit & Pull Request Guidelines

This repository has no established commit history yet. Use short imperative commit messages, such as `Add opencode adapter actions` or `Wire instance API routes`.

Pull requests should include a concise summary, notable behavior changes, and the exact verification commands run. For API changes, include example routes or payloads. For TUI/PTY changes, describe the manual smoke test and platform used.

## Security & Configuration Tips

The daemon listens on `127.0.0.1` by default. Do not expose it on public interfaces without adding authentication and input restrictions. Treat RPC input as equivalent to typing into the wrapped agent's terminal.
