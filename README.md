# t-acp

`t-acp` 是一个 Rust 编写的本地控制层：它把终端里的 TUI agent 包在前台运行，同时暴露一个本地 HTTP API，用来查看实例状态、读取最近屏幕内容，并向 agent 注入输入或适配器动作。

当前它最适合这样的场景：

- 你仍然希望直接在当前终端里使用 `opencode`、`claude-code`、`codex` 等 TUI agent。
- 你还希望从另一个本地进程或脚本里观测它们的状态。
- 你需要一个简单的本地 RPC 接口来发送提示词、处理权限弹窗或终止会话。

## 特性

- 前台运行 agent，保留原始 TUI 交互体验。
- 自动启动本地 daemon，并把实例注册到内存注册表。
- 通过 PTY 转发标准输入、标准输出和窗口大小变化。
- 提供本地 HTTP API：列出实例、查看详情、发送输入、执行动作、终止实例。
- 对 `opencode` 提供额外适配能力：
  - 检测权限确认界面
  - `send-prompt` 使用 bracketed paste 注入多行提示
  - 支持批准 / 拒绝权限弹窗
- 对其他 agent 提供通用适配器回退。

## 项目结构

```text
src/main.rs      CLI 入口
src/wrapper.rs   前台包装、daemon 自启动、PTY 与 RPC 转发
src/daemon.rs    本地 HTTP 控制服务与实例注册表
 src/adapters/    adapter trait 与具体实现，当前重点支持 opencode
src/pty.rs       Unix PTY 启动与 resize
src/http.rs      daemon 客户端
src/api.rs       HTTP 请求 / 响应结构
src/util.rs      小型辅助函数
plans/           设计与实现计划
```

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

## 用法

### 1. 启动 daemon

手动启动：

```bash
cargo run -- daemon
```

指定地址：

```bash
cargo run -- daemon --addr 127.0.0.1:49001
```

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
5. daemon 会记录实例元信息、状态和最近屏幕内容。

## 快速示例

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

终止实例：

```bash
curl -X DELETE \
  http://127.0.0.1:48974/agents/<instance_id>
```

## HTTP API

所有接口默认监听在 `http://127.0.0.1:48974`。

### 对外接口

#### `GET /health`

健康检查。

响应示例：

```json
{
  "ok": true
}
```

#### `GET /agents`

列出当前注册的 agent 实例。

#### `GET /agents/{instance_id}`

查看单个实例详情。

#### `POST /agents/{instance_id}/input`

向实例注入原始字节输入。请求体直接作为 PTY 输入写入，不要求 JSON。

适合发送：

- 普通文本
- `\n` / `\r`
- 控制字符，例如 `Ctrl+C` 对应 `0x03`

#### `POST /agents/{instance_id}/actions/send-prompt`

发送“提示词”动作。

- 对 `opencode`：使用 bracketed paste 包裹请求体，并自动回车提交。
- 对 generic adapter：如果请求体末尾没有换行，会自动补一个换行。

#### `POST /agents/{instance_id}/actions/approve-permission`

批准权限请求。

- 对 `opencode`：只有检测到权限弹窗时才会排队执行。
- 当前实现发送回车确认。

#### `POST /agents/{instance_id}/actions/reject-permission`

拒绝权限请求。

- 对 `opencode`：只有检测到权限弹窗时才会排队执行。
- 当前实现发送 `Esc`。

#### `POST /agents/{instance_id}/actions/previous-model`

切换到上一个模型。

- 对 `opencode`：发送 `Shift+F2`
- generic adapter 当前返回 `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/next-model`

切换到下一个模型。

- 对 `opencode`：发送 `F2`
- generic adapter 当前返回 `501 unsupported_action`

#### `POST /agents/{instance_id}/actions/switch-model`

预留接口。

- `opencode` 当前返回 `501 unsupported_action`
- generic adapter 也尚未实现

#### `DELETE /agents/{instance_id}`

向实例发送 `Ctrl+C`，用于请求终止当前前台 agent。

### 内部接口

以下接口主要由 wrapper 与 daemon 自身使用，不建议外部调用：

- `POST /internal/agents/register`
- `GET /internal/agents/{instance_id}/ws`
- `POST /internal/agents/{instance_id}/exit`

当前 wrapper 与 daemon 之间通过 `/internal/agents/{instance_id}/ws` 传输：

- wrapper 通过 WebSocket 上报 PTY output
- wrapper 通过 WebSocket 上报 resize、focus 等内部运行时事件
- daemon 通过同一条 WebSocket 下发待执行命令

## 数据模型

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
  "focused": true,
  "exit_status": null,
  "created_at_ms": 1716620000000,
  "updated_at_ms": 1716620001234,
  "screen_tail": "...recent terminal screen contents..."
}
```

字段说明：

- `status`: `starting`、`ready`、`busy`、`blocked`、`exited`
- `ui_mode`: `unknown`、`normal`、`input`、`permission_prompt`、`model_picker`
- `blocking_reason`: 当前仅在识别到权限阻塞时可能为 `permission`
- `current_agent`: 当前输入框底部状态条里的 agent 名，例如 `Build`
- `current_model`: 当前模型名；对 `opencode` 会优先解析底部状态条，回退到类似 `Model: ...` 的文本
- `current_provider`: 当前 provider 名，例如 `GitHub Copilot`
- `current_reasoning_effort`: 当前思考强度，例如 `high`
- `current_context_window`: 右下角显示的当前上下文长度，例如 `42.6K`
- `current_context_usage_percent`: 右下角显示的上下文占用百分比，例如 `21`
- `focused`: 外层终端当前是否处于 focus 状态
- `screen_tail`: daemon 维护的最近终端屏幕文本，用于状态观察和适配器判断

如果在 `tmux` 里使用时发现 `focused` 状态不准确，需要启用 `tmux` 的 focus 事件转发：`set -g focus-events on`。

## 适配器说明

### opencode

当前为 `opencode` 提供了更具体的运行时识别逻辑：

- 识别权限弹窗
- 粗略识别 model picker
- 根据屏幕文本推断 `starting` / `ready` / `busy` / `blocked`
- 从输入框底部状态条提取 agent / model / provider / thinking effort
- 从右下角状态区提取 context length / context usage percent
- 回退提取类似 `Model: ...` 的模型信息

### generic

除 `opencode` 以外的命令默认使用 `generic` 适配器：

- 可以注册实例
- 可以发送原始输入
- 可以使用 `send-prompt`
- 不提供可靠的权限界面识别和 model 切换能力

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

## 当前限制

- daemon 注册表是内存态的，重启后实例信息不会持久化。
- `screen_tail` 基于终端屏幕内容，不是完整日志流。
- `switch-model` 还未实现。
- 远程 `resize` API 还未接线。
- 适配器状态识别目前以启发式文本判断为主，尤其是 `opencode` 的 UI 检测仍然可能误判。
- 服务默认只监听本地回环地址，没有认证机制，不应直接暴露到公网。

## 后续可扩展方向

- 增加更多 agent 的专用适配器
- 为实例增加持久化和历史记录
- 提供更稳定的事件流接口
- 完善 model 切换和远程 resize
- 补充更多端到端 smoke tests
