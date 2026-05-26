# Repository Guidelines

See `AGENTS.md` for the full repository conventions.

## Architecture Constraints

t-acp is a non-invasive wrapper. Do not require or implement hooks, plugins, patches, or other in-process extensions for wrapped CLI/TUI tools such as opencode, Claude Code, or Codex.

The required base layer is wrapper-owned PTY observation plus wrapper-owned stdin/action control. PTY screen parsing is the portable ground truth for what the user can see, and stdin control is the portable control plane for approve/reject/select/custom-answer interactions.

All wrapper-to-daemon runtime traffic for PTY output, resize, and queued input must use the internal WebSocket at `/internal/agents/:instance_id/ws`. Do not add internal HTTP output, resize, or command-polling fallbacks unless compatibility explicitly requires it.

CLI-native structured outputs may be used only as optional adapter enrichment when they already exist without modifying the wrapped tool, for example documented local APIs, JSON logs, debug streams, or trace files. Do not make provider API response interception, MITM traffic analysis, or model-response parsing the primary state source for local TUI interaction state.

Interaction ids must be semantic and redraw-stable. Do not include raw terminal frames, cursor positions, spinner output, or other volatile render noise in `interaction_request.id`.

Daemon behavior should be robust under stale runtime state: recover poisoned registry locks, keep wrapper-started daemons alive after short wrapper commands exit, and restart the daemon after changing embedded HTML templates.
