# t-acp 架构说明

[English](./architecture.md)

本文描述当前 `t-acp` 的实际架构。`t-acp` 是一个非侵入式的本地 wrapper：它观察和控制真实 PTY 会话，不要求被包装的 CLI/TUI 提供 hook、插件、补丁，也不把 provider API 拦截或模型响应解析作为基础方案。

## 设计目标

- 保留原生前台 TUI 体验。
- 让每个被包装的实例都能被本地 API 观察和控制。
- 以终端屏幕解析作为人类可见状态的可移植事实来源。
- 把需要人类介入的 prompt 抽象成稳定的结构化请求和提交。
- 把 agent 专有行为隔离在 adapter 里。
- 先做好 `opencode`，同时保留未来扩展到 `claude-code`、`codex` 和其他 TUI agent 的边界。

## 运行时形态

`t-acp` 由两个协作的运行时组成：

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

wrapper 保持在前台，拥有目标 agent 的 PTY。daemon 在后台运行，维护所有已注册实例的共享观察和控制状态。

## 主要组件

### CLI 与 Wrapper

主要实现于 `src/main.rs` 和 `src/wrapper.rs`。

职责：

- 分发 `t-acp daemon` 和 `t-acp <agent> [args...]`。
- 在启动被包装 agent 前确保 daemon 可用。
- 保留调用方当前工作目录，传给被包装命令。
- 在 PTY 中启动目标命令。
- 把本地键盘和终端输入转发到 PTY。
- 把 PTY 输出同时镜像到用户终端和 daemon。
- 把本地终端 resize、focus 事件转发给 daemon。
- 从 daemon 接收远程命令队列，并写入 PTY。

### PTY Runtime

主要实现于 `src/pty.rs`。

职责：

- 创建 Unix PTY。
- 在指定工作目录里启动子进程。
- 根据 wrapper 本地 `SIGWINCH` 事件调整 PTY 尺寸。
- 向 wrapper 提供 PTY 读写句柄。

### Daemon

主要实现于 `src/daemon.rs`。

职责：

- 维护活跃实例的内存注册表。
- 存储实例元信息、生命周期状态、终端 focus、退出状态。
- 为每个实例维护 vt100 screen snapshot。
- 维护 raw PTY 和 observation event 的环形缓冲，用于调试。
- 在 screen 更新后运行 adapter 识别逻辑。
- 暴露公开 HTTP、SSE 和 HTML 观察接口。
- 将输入和动作字节排队，等待 wrapper 取走并写回 PTY。

注册表目前只是内存态。daemon 重启后，已跟踪实例的元信息和观察缓冲都会丢失。

### 内部运行时通道

协议定义在 `src/internal.rs`，桥接逻辑在 `src/http.rs` 和 `src/daemon.rs`。

wrapper 与 daemon 之间所有运行时流量都走：

```text
GET /internal/agents/{instance_id}/ws
```

这条 WebSocket 承载：

- wrapper 到 daemon 的 PTY output frame
- wrapper 到 daemon 的 terminal resize frame
- wrapper 到 daemon 的 focus frame
- daemon 到 wrapper 的 queued command frame

内部 HTTP 只用于注册和退出上报：

- `POST /internal/agents/register`
- `POST /internal/agents/{instance_id}/exit`

除非有明确兼容性需求，不应再新增内部 HTTP output、resize 或 command polling fallback。

### Adapter 层

实现位于 `src/adapters/`。

所有 agent 专有行为都应该收敛到 adapter 边界内：

- 从解析后的 screen 识别 UI mode 和 blocking state
- 尽量提取 model、provider、context 等运行时元数据
- 生成高层 action 对应的输入字节序列
- 将需要人类介入的 prompt 解析为结构化 `interaction_request`
- 通过确定性的 stdin 序列提交结构化交互

当前 adapter：

- `opencode`：专用 adapter，支持结构化 permission/question 解析和模型快捷键。
- `generic`：`claude-code`、`codex` 和未知命令的回退 adapter。当前只支持注册、raw input 和简单 prompt 发送。

adapter 可以把 CLI 原生结构化输出作为可选增强，但基础状态来源仍然是 PTY screen。

### API 类型

主要实现于 `src/api.rs` 和 `src/interactions.rs`。

关键公开结构：

- `AgentView`：`/agents` 返回的实例状态。
- `ObservationView`：用于调试的 screen snapshot、event tail、raw PTY tail。
- `InteractionRequest`：从 TUI screen 里识别到的结构化人类输入请求。
- `InteractionOption`：某个交互里的可选项。
- `SubmitInteractionRequest`：用户/API 对当前可见交互的提交。

interaction id 必须是语义稳定、可跨 redraw 复用的。它应该来自 source、kind、title、subject、prompt、options 等稳定字段，不能包含 raw frame text、cursor position、spinner output 或其他易变渲染噪声。

## 数据流

### 观察流

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

这条路径让用户继续使用原生 TUI，同时本地 controller 可以通过 HTTP、SSE 和 observation page 观察同一份可见状态。

### 人类交互流

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

stale-id 检查用于避免旧浏览器页面或自动化客户端审批一个已经消失的 prompt。

### Action 流

高层 action 与结构化 interaction 使用同一条队列：

```text
POST /agents/{id}/actions/send-prompt
POST /agents/{id}/actions/approve-permission
POST /agents/{id}/actions/reject-permission
POST /agents/{id}/actions/previous-model
POST /agents/{id}/actions/next-model
```

daemon 会要求当前 adapter 构造确定性的输入序列。如果当前 UI 状态下无法安全执行，adapter 会返回结构化错误，例如 `ui_not_detected` 或 `unsupported_action`。

## 公开接口

默认地址：

```text
http://127.0.0.1:48974
```

读取接口：

- `GET /health`
- `GET /agents`
- `GET /agents/{instance_id}`
- `GET /observe`
- `GET /agents/{instance_id}/observe`
- `GET /agents/{instance_id}/observations`
- `GET /agents/events/stream`
- `GET /agents/{instance_id}/events/stream`

写入接口：

- `POST /agents/{instance_id}/input`
- `POST /agents/{instance_id}/interaction`
- `POST /agents/{instance_id}/actions/send-prompt`
- `POST /agents/{instance_id}/actions/approve-permission`
- `POST /agents/{instance_id}/actions/reject-permission`
- `POST /agents/{instance_id}/actions/previous-model`
- `POST /agents/{instance_id}/actions/next-model`
- `POST /agents/{instance_id}/actions/switch-model`
- `DELETE /agents/{instance_id}`

`/observe` 是零构建 HTML 观察面板，模板来自 `assets/observe.html` 并被嵌入二进制。修改模板后需要重新 build 并重启 daemon。

## 状态模型

重要 `AgentView` 字段：

- `status`: `starting`、`ready`、`busy`、`blocked`、`exited`
- `ui_mode`: `unknown`、`normal`、`input`、`permission_prompt`、`model_picker`
- `blocking_reason`: 当前最常见是 `permission`
- `need_interactive`: 是否需要人类介入
- `interactive_kind`: 高层交互类型，例如 `permission` 或 `question`
- `interaction_request`: 当前可见的结构化交互请求
- `focused`: 外层终端是否报告 focus
- `screen_tail`: 最近解析出的屏幕文本

调试时，`ObservationView` 还会提供完整 screen lines、cursor position、最近 observation events 和 raw PTY tail。

## 扩展新 Adapter

为新的 CLI/TUI 增加专用 adapter 时：

1. 保持被包装 CLI 不变，不要求 hook 或插件。
2. 在 `src/adapters/` 增加 screen recognition。
3. 把可见阻塞 prompt 解析成 `InteractionRequest`。
4. 使用语义稳定、可跨 redraw 的 interaction id。
5. 通过确定性的 stdin 字节实现提交。
6. 当预期 UI 不可见时返回结构化错误。
7. 用典型 screen fixture 增加单元测试。
8. 把 CLI 原生元数据作为可选增强，不作为基础控制路径。

## 当前限制

- daemon 注册表和观察缓冲不持久化。
- screen parsing 仍是启发式，可能漏掉 UI 变体。
- 当前只有 `opencode` 有结构化交互支持的专用 adapter。
- `switch-model` 已暴露但尚未实现。
- 目前没有公开 remote resize API；wrapper 本地 resize 通过内部通道同步。
- 服务没有认证，应保持绑定在 loopback，除非后续增加鉴权和输入限制。
