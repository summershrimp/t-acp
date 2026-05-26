# t-acp

[English](./README.md) | [架构说明](./docs/architecture.zh.md)

`t-acp` 是一个面向终端 TUI agent 的本地控制层。

它让你继续在当前终端里直接使用 `opencode`、`claude-code`、`codex` 这类交互式 agent，同时额外提供一个本地 HTTP API，供脚本、自动化流程或其他本地进程查询状态、读取最近屏幕内容、发送输入，以及触发适配器动作。

## 项目简介

适合这样的场景：

- 你希望保留原始 TUI 使用体验，而不是把 agent 改造成纯 API 模式。
- 你希望从另一个本地进程观察 agent 当前是否忙碌、是否卡在权限确认、当前模型是什么。
- 你希望用一个简单的本地接口来发 prompt、批准权限弹窗、切换模型或终止实例。

它的定位不是托管远程会话，而是在本机把“前台可交互终端”和“可编程控制接口”组合起来。

运行时结构和扩展边界见 [架构说明](./docs/architecture.zh.md)。

## 能力概览

- 前台运行 agent，保留原始 PTY/TUI 交互体验。
- 自动确保本地 daemon 可用，并把新实例注册进去。
- 暴露本地 HTTP API：列出实例、查看详情、发送输入、提交结构化交互、执行动作、终止实例。
- 维护最近终端 screen snapshot、raw PTY tail、observation events，以及本地 HTML observation plane。
- 暴露运行时元数据：当前 agent、模型、provider、thinking effort、context 使用量、focus 状态。
- 对 `opencode` 提供专用适配能力：结构化 permission/question 解析、prompt 注入、interaction 提交、模型切换快捷键。
- 对 `claude-code`、`codex` 和其他未知命令提供通用适配器回退。

## 当前支持

### `opencode`

- 识别权限弹窗
- 将 external-directory、doom-loop、question 等需要人类介入的界面识别为结构化 `interaction_request`
- `send-prompt` 使用 bracketed paste 注入多行内容并自动提交
- 支持 `approve-permission` / `reject-permission`
- 支持统一的 `POST /agents/{instance_id}/interaction`，用于 allow-once、allow-always、reject、选项选择和自定义回答
- 支持 `previous-model` / `next-model`
- 尝试提取运行时元数据：agent、model、provider、reasoning effort、context usage

### generic 适配器

当前这些命令会走 `generic` 适配器：

- `claude-code`
- `codex`
- 其他未实现专用适配器的命令

generic 适配器支持：

- 注册实例
- 发送原始输入
- `send-prompt`

generic 适配器当前不提供可靠的权限界面识别和模型切换能力。

## 依赖与环境

- Rust 2024 edition
- Cargo
- 本机可执行的终端 agent，例如 `opencode`

默认监听地址：`127.0.0.1:48974`

可以通过环境变量覆盖：

```bash
export T_ACP_ADDR=127.0.0.1:49001
```

日志过滤使用 `RUST_LOG` 控制，例如：

```bash
RUST_LOG=info cargo run -- daemon
```

## 构建

```bash
cargo build
```

## 快速开始

### 1. 启动 daemon

手动启动：

```bash
cargo run -- daemon
```

指定地址：

```bash
cargo run -- daemon --addr 127.0.0.1:49001
```

通常也可以不手动启动。直接通过 wrapper 运行 agent 时，`t-acp` 会先检查 daemon；如果还没启动，会自动在后台拉起。

### 2. 通过 wrapper 启动 agent

```bash
cargo run -- opencode
```

传递额外参数：

```bash
cargo run -- opencode --model gpt-5
```

也可以包装其他命令：

```bash
cargo run -- claude-code
cargo run -- codex
cargo run -- /path/to/custom-agent
```

当你以 `t-acp <agent> ...` 方式运行时：

1. wrapper 会检查 daemon 是否健康。
2. 如果 daemon 未启动，会自动在后台拉起一个。
3. agent 会在 PTY 中以前台方式运行。
4. 屏幕输出会继续显示在当前终端，同时同步给 daemon。
5. PTY output、resize、focus 和远程命令队列通过 `/internal/agents/{instance_id}/ws` 这条内部 WebSocket 流动。
6. daemon 会记录实例元信息、状态、最近屏幕内容、raw PTY tail、observation events、结构化交互和可识别的运行时元数据。

## 最常用的调用方式

启动 agent：

```bash
cargo run -- opencode
```

查看实例列表：

```bash
curl http://127.0.0.1:48974/agents
```

向某个实例发送原始输入：

```bash
curl -X POST \
  --data-binary $'hello from api\n' \
  http://127.0.0.1:48974/agents/<instance_id>/input
```

通过适配器发送 prompt：

```bash
curl -X POST \
  --data-binary 'Summarize the current repo structure.' \
  http://127.0.0.1:48974/agents/<instance_id>/actions/send-prompt
```

如果 `opencode` 出现权限确认界面，批准该操作：

```bash
curl -X POST \
  http://127.0.0.1:48974/agents/<instance_id>/actions/approve-permission
```

提交当前可见的结构化交互：

```bash
curl -X POST \
  -H 'content-type: application/json' \
  -d '{"interaction_id":"<interaction_id>","option_key":"allow_once","custom_answer":null}' \
  http://127.0.0.1:48974/agents/<instance_id>/interaction
```

打开 observation plane：

```bash
open http://127.0.0.1:48974/observe
```

切到下一个模型：

```bash
curl -X POST \
  http://127.0.0.1:48974/agents/<instance_id>/actions/next-model
```

终止实例：

```bash
curl -X DELETE \
  http://127.0.0.1:48974/agents/<instance_id>
```

## 对外 API

所有接口默认监听在 `http://127.0.0.1:48974`。

### 读取接口

#### `GET /health`

健康检查。

响应示例：

```json
{
  "ok": true
}
```

#### `GET /agents`

列出当前注册的实例。

响应示例：

```json
{
  "agents": []
}
```

#### `GET /agents/{instance_id}`

查看单个实例详情。

#### `GET /observe`

返回零构建的本地 HTML observation plane。页面会列出已注册 agents，支持选择实例，展示当前 screen、结构化人类交互控件、event timeline 和 raw PTY tail。

模板来自 `assets/observe.html` 并被嵌入二进制；修改该文件后需要重新 build 并重启 daemon。

#### `GET /agents/{instance_id}/observe`

返回同一个 observation plane，并预选指定实例。

#### `GET /agents/{instance_id}/observations`

返回某个实例的 observation JSON：

- `agent`: 与 `GET /agents/{instance_id}` 相同的实例对象
- `screen`: daemon 的 vt100 snapshot，包含尺寸、cursor、lines 和完整文本
- `events`: 最近的 observation events，例如 `pty_output_received`、`state_changed`、`interaction_detected`
- `raw_tail_hex`: 最近 raw PTY bytes 的十六进制表示
- `raw_tail_utf8_lossy`: 最近 raw PTY bytes 的 UTF-8 lossy 解码

#### `GET /agents/{instance_id}/events/stream`

以 Server-Sent Events 流式返回单个实例的状态变化。

#### `GET /agents/events/stream`

以 Server-Sent Events 流式返回全部实例的状态变化。

### 写入接口

除 `GET /health`、`GET /agents`、`GET /agents/{instance_id}` 之外，其余对外写接口在成功时都会返回 `202 Accepted`：

```json
{
  "queued": true,
  "adapter": "opencode"
}
```

说明：

- `queued: true` 表示命令已被排队或已通过内部通道下发
- `adapter` 表示这次动作由哪个适配器生成
- 原始 `input` 和 `DELETE /agents/{instance_id}` 这类非适配器动作会返回 `"adapter": null`

#### `POST /agents/{instance_id}/input`

向实例注入原始字节输入。请求体会直接写入 PTY，不要求 JSON。

适合发送：

- 普通文本
- `\n` / `\r`
- 控制字符，例如 `Ctrl+C` 对应 `0x03`

#### `POST /agents/{instance_id}/actions/send-prompt`

发送“提示词”动作。

- 对 `opencode`：使用 bracketed paste 包裹请求体，并自动回车提交
- 对 generic adapter：如果请求体末尾没有换行，会自动补一个换行

#### `POST /agents/{instance_id}/actions/approve-permission`

批准权限请求。

- 对 `opencode`：只有检测到权限弹窗时才会执行
- 当前实现发送回车确认

#### `POST /agents/{instance_id}/actions/reject-permission`

拒绝权限请求。

- 对 `opencode`：只有检测到权限弹窗时才会执行
- 当前实现发送 `Esc`

#### `POST /agents/{instance_id}/interaction`

提交当前可见的结构化人类交互。如果 `interaction_id` 已经不匹配当前可见的 `interaction_request.id`，daemon 会返回 `409 stale_interaction`，避免旧页面或旧自动化请求误操作。

请求体：

```json
{
  "interaction_id": "opencode-external_directory-...",
  "option_key": "allow_once",
  "custom_answer": null
}
```

规则：

- `option_key` 和 `custom_answer` 必须二选一。
- permission interaction 支持 `allow_once`、`allow_persist`、`deny` 等 option/action。
- question interaction 可以提交 option key；如果 `custom_answer_allowed` 为 `true`，也可以提交 `custom_answer`。
- `interaction_request.id` 是语义稳定、可跨 redraw 的，不包含 raw screen noise、cursor position 或 spinner frame。

#### `POST /agents/{instance_id}/actions/previous-model`

切换到上一个模型。

- 对 `opencode`：发送 `Shift+F2`
- generic adapter 当前返回 `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/next-model`

切换到下一个模型。

- 对 `opencode`：发送 `F2`
- generic adapter 当前返回 `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/switch-model`

预留接口，请求体可用于承载目标模型标识。

- `opencode` 当前返回 `501 unsupported_action`
- generic adapter 当前也未实现

#### `DELETE /agents/{instance_id}`

向实例发送 `Ctrl+C`，用于请求终止当前前台 agent。

## 实例对象说明

`GET /agents` 与 `GET /agents/{instance_id}` 返回的实例对象形如：

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
  "need_interactive": false,
  "interactive_kind": null,
  "interaction_request": null,
  "focused": true,
  "exit_status": null,
  "created_at_ms": 1716620000000,
  "updated_at_ms": 1716620001234,
  "screen_tail": "...recent terminal screen contents..."
}
```

主要字段：

- `agent_kind`: 命令名归一化后的实例类型，例如 `opencode`、`claude_code`、`codex`
- `adapter`: 实际使用的适配器名；当前只有 `opencode` 会返回专用适配器，其余通常为 `generic`
- `status`: `starting`、`ready`、`busy`、`blocked`、`exited`
- `ui_mode`: `unknown`、`normal`、`input`、`permission_prompt`、`model_picker`
- `blocking_reason`: 当前仅在识别到权限阻塞时可能为 `permission`
- `current_agent`: 当前输入框底部状态条里的 agent 名，例如 `Build`
- `current_model`: 当前模型名
- `current_provider`: 当前 provider 名，例如 `GitHub Copilot`
- `current_reasoning_effort`: 当前思考强度，例如 `high`
- `current_context_window`: 当前上下文长度，例如 `42.6K`
- `current_context_usage_percent`: 当前上下文占用百分比，例如 `21`
- `need_interactive`: adapter 是否认为需要人类介入
- `interactive_kind`: 高层交互类型，例如 `permission` 或 `question`
- `interaction_request`: 当前可见的结构化交互 payload，observation plane 会根据它渲染可点击控件
- `focused`: 外层终端当前是否处于 focus 状态
- `screen_tail`: daemon 维护的最近终端屏幕文本，用于状态观察和适配器判断

`interaction_request` 示例：

```json
{
  "id": "opencode-question-7b7c1a2f59c770dd",
  "kind": "question",
  "source": "opencode",
  "title": "Question",
  "subject": null,
  "prompt": "Which implementation path should I take?",
  "options": [
    {
      "key": "1",
      "label": "Small focused patch",
      "selected": true,
      "action": null
    }
  ],
  "custom_answer_allowed": true,
  "confidence": 90,
  "evidence": [
    {
      "label": "source",
      "value": "pty_screen"
    }
  ],
  "raw": "...recent interaction screen block..."
}
```

## 关于 focus 状态

`focused` 依赖终端支持 focus reporting。

wrapper 启动时会尝试开启这项能力；如果终端或多路复用器不支持，`focused` 可能一直保持默认值。

如果你在 `tmux` 里使用，建议开启：

```tmux
set -g focus-events on
```

## 错误语义

常见错误包括：

- `404 not_found`: 实例不存在
- `409 process_exited`: 实例已经退出，不能再写入动作
- `409 ui_not_detected`: 适配器要求的界面当前不可见，例如并没有权限弹窗
- `400 bad_request`: 请求体不合法，例如空 prompt
- `409 stale_interaction`: 提交的交互已经不可见，或 id 已变化
- `501 unsupported_action`: 当前适配器还不支持该动作

## 当前限制

- daemon 注册表是内存态的，重启后实例信息不会持久化
- `screen_tail` 基于终端屏幕内容，不是完整日志流
- observation-plane raw PTY tail 和 events 是内存环形缓冲，daemon 重启后不会保留
- `switch-model` 还未实现
- 当前没有公开 remote resize API；wrapper 本地 `SIGWINCH` resize 会通过内部 WebSocket 同步
- `focused` 依赖终端和 `tmux`/多路复用器正确转发 focus 事件
- 适配器状态识别目前以启发式文本判断为主，尤其是 `opencode` 的 UI 检测仍然可能误判
- 服务默认只监听本地回环地址，没有认证机制，不应直接暴露到公网

## 开发与测试

格式化：

```bash
cargo fmt
cargo fmt --check
```

运行测试：

```bash
cargo test
```

手动 smoke test：

```bash
target/debug/t-acp daemon --addr 127.0.0.1:49001
T_ACP_ADDR=127.0.0.1:49001 target/debug/t-acp /bin/cat
```

在另一个终端里发送输入：

```bash
curl -X POST \
  --data-binary $'ping\n' \
  http://127.0.0.1:49001/agents/<instance_id>/input
```

## 项目结构

```text
src/main.rs              CLI 入口
src/wrapper.rs           前台包装、daemon 自启动、PTY 与 RPC 转发
src/daemon.rs            本地 HTTP 控制服务与实例注册表
src/adapters/            adapter trait 与具体实现
src/adapters/generic.rs  通用适配器
src/adapters/opencode.rs opencode 适配器与元数据提取
src/interactions.rs      interaction id、evidence helper 与提交校验
src/pty.rs               Unix PTY 启动与 resize
src/http.rs              daemon 客户端
src/api.rs               HTTP 请求 / 响应结构
src/internal.rs          wrapper 与 daemon 间的内部 WebSocket 消息
src/util.rs              小型辅助函数
assets/observe.html      嵌入式 observation-plane HTML 模板
docs/architecture.zh.md  当前架构说明
plans/                   设计与实现计划
```

## 内部机制说明

以下接口主要由 wrapper 与 daemon 自身使用，不建议外部调用：

- `POST /internal/agents/register`
- `GET /internal/agents/{instance_id}/ws`（WebSocket upgrade）
- `POST /internal/agents/{instance_id}/exit`

当前 wrapper 与 daemon 之间通过 `/internal/agents/{instance_id}/ws` 传输：

- wrapper 上报 PTY output
- wrapper 上报 resize、focus 等内部运行时事件
- daemon 通过同一条 WebSocket 下发待执行命令

也就是说，运行时数据面现在走内部 WebSocket；内部 HTTP 主要只负责注册和退出上报。
