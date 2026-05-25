use crate::adapters::{self, Adapter, AdapterError};
use crate::api::{
    ActionQueued, AgentList, AgentView, ErrorBody, ExitRequest, HealthResponse,
    RegisterAgentRequest,
};
use crate::internal::{InternalWsClientMessage, InternalWsServerMessage};
use crate::util::now_millis;
use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::info;

type SharedRegistry = Arc<Mutex<Registry>>;

#[derive(Default)]
struct Registry {
    instances: HashMap<String, AgentInstance>,
}

struct AgentInstance {
    id: String,
    agent_kind: String,
    adapter: &'static dyn Adapter,
    pid: Option<u32>,
    cwd: String,
    command: String,
    status: String,
    ui_mode: String,
    blocking_reason: Option<String>,
    current_agent: Option<String>,
    current_model: Option<String>,
    current_provider: Option<String>,
    current_reasoning_effort: Option<String>,
    current_context_window: Option<String>,
    current_context_usage_percent: Option<u8>,
    focused: bool,
    exit_status: Option<String>,
    screen: vt100::Parser,
    screen_tail: String,
    command_queue: VecDeque<Vec<u8>>,
    ws_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    created_at_ms: u128,
    updated_at_ms: u128,
}

pub async fn run(addr: &str) -> Result<()> {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let app = Router::new()
        .route("/health", get(health))
        .route("/agents", get(list_agents))
        .route("/agents/{instance_id}", get(get_agent).delete(delete_agent))
        .route("/agents/{instance_id}/input", post(post_input))
        .route(
            "/agents/{instance_id}/actions/send-prompt",
            post(action_send_prompt),
        )
        .route(
            "/agents/{instance_id}/actions/approve-permission",
            post(action_approve_permission),
        )
        .route(
            "/agents/{instance_id}/actions/reject-permission",
            post(action_reject_permission),
        )
        .route(
            "/agents/{instance_id}/actions/previous-model",
            post(action_previous_model),
        )
        .route(
            "/agents/{instance_id}/actions/next-model",
            post(action_next_model),
        )
        .route(
            "/agents/{instance_id}/actions/switch-model",
            post(action_switch_model),
        )
        .route("/internal/agents/register", post(register_agent))
        .route("/internal/agents/{instance_id}/ws", any(connect_agent_ws))
        .route("/internal/agents/{instance_id}/exit", post(mark_exited))
        .with_state(registry);

    let addr = addr
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid listen address {addr}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!("t-acp daemon listening on http://{addr}");

    axum::serve(listener, app)
        .await
        .context("axum server failed")
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn list_agents(State(registry): State<SharedRegistry>) -> Json<AgentList> {
    let registry = registry.lock().expect("registry lock poisoned");
    Json(AgentList {
        agents: registry
            .instances
            .values()
            .map(AgentInstance::view)
            .collect(),
    })
}

async fn get_agent(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<Json<AgentView>, ApiError> {
    let registry = registry.lock().expect("registry lock poisoned");
    let instance = registry
        .instances
        .get(&instance_id)
        .ok_or(ApiError::NotFound)?;

    Ok(Json(instance.view()))
}

async fn post_input(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_command(&registry, &instance_id, body.to_vec(), None)
}

async fn action_send_prompt(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance.adapter.send_prompt(&body)
    })
}

async fn action_approve_permission(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance
            .adapter
            .approve_permission(instance.screen_tail.as_bytes())
    })
}

async fn action_reject_permission(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance
            .adapter
            .reject_permission(instance.screen_tail.as_bytes())
    })
}

async fn action_previous_model(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance.adapter.previous_model()
    })
}

async fn action_next_model(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance.adapter.next_model()
    })
}

async fn action_switch_model(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_adapter_action(&registry, &instance_id, |instance| {
        instance.adapter.switch_model(&body)
    })
}

async fn delete_agent(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_command(&registry, &instance_id, b"\x03".to_vec(), None)
}

async fn register_agent(
    State(registry): State<SharedRegistry>,
    Json(request): Json<RegisterAgentRequest>,
) -> Result<(StatusCode, Json<AgentView>), ApiError> {
    let agent_kind = adapters::canonical_agent_kind(&request.agent_kind);
    let adapter = adapters::for_agent_kind(&agent_kind);
    let observation = adapter.observe(&[]);
    let now = now_millis();
    let instance = AgentInstance {
        id: request.id.clone(),
        agent_kind: agent_kind.clone(),
        adapter,
        pid: request.pid,
        cwd: request.cwd,
        command: request.command,
        status: observation.status.as_str().to_string(),
        ui_mode: observation.ui_mode.as_str().to_string(),
        blocking_reason: observation.blocking_reason,
        current_agent: observation.current_agent,
        current_model: observation.current_model,
        current_provider: observation.current_provider,
        current_reasoning_effort: observation.current_reasoning_effort,
        current_context_window: observation.current_context_window,
        current_context_usage_percent: observation.current_context_usage_percent,
        focused: false,
        exit_status: None,
        screen: vt100::Parser::new(request.rows, request.cols, 2000),
        screen_tail: String::new(),
        command_queue: VecDeque::new(),
        ws_tx: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    let view = instance.view();

    registry
        .lock()
        .expect("registry lock poisoned")
        .instances
        .insert(request.id, instance);

    Ok((StatusCode::CREATED, Json(view)))
}

async fn connect_agent_ws(
    ws: WebSocketUpgrade,
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<Response, ApiError> {
    {
        let registry = registry.lock().expect("registry lock poisoned");
        let instance = registry
            .instances
            .get(&instance_id)
            .ok_or(ApiError::NotFound)?;

        if instance.status == "exited" {
            return Err(ApiError::ProcessExited);
        }
    }

    Ok(ws.on_upgrade(move |socket| handle_agent_ws(registry, instance_id, socket)))
}

fn render_screen_tail(parser: &vt100::Parser) -> String {
    let screen = parser.screen();
    let (_, cols) = screen.size();
    screen.rows(0, cols).collect::<Vec<_>>().join("\n")
}

async fn mark_exited(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    Json(request): Json<ExitRequest>,
) -> Result<StatusCode, ApiError> {
    let mut registry = registry.lock().expect("registry lock poisoned");
    let instance = registry
        .instances
        .get_mut(&instance_id)
        .ok_or(ApiError::NotFound)?;

    instance.status = "exited".to_string();
    instance.ui_mode = "unknown".to_string();
    instance.blocking_reason = None;
    instance.exit_status = Some(request.status);
    instance.updated_at_ms = now_millis();

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_agent_ws(registry: SharedRegistry, instance_id: String, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    {
        let mut registry = registry.lock().expect("registry lock poisoned");
        let Some(instance) = registry.instances.get_mut(&instance_id) else {
            return;
        };
        instance.ws_tx = Some(tx);

        while let Some(command) = instance.command_queue.pop_front() {
            if let Some(ws_tx) = &instance.ws_tx {
                if ws_tx.send(command).is_err() {
                    instance.ws_tx = None;
                    break;
                }
            }
        }
    }

    let send_task = tokio::spawn(async move { send_ws_commands(&mut sender, &mut rx).await });

    while let Some(message) = receiver.next().await {
        let Ok(message) = message else {
            break;
        };

        match message {
            Message::Text(text) => {
                if let Ok(frame) = serde_json::from_str::<InternalWsClientMessage>(&text) {
                    handle_ws_client_message(&registry, &instance_id, frame);
                }
            }
            Message::Binary(binary) => {
                if let Ok(frame) = serde_json::from_slice::<InternalWsClientMessage>(&binary) {
                    handle_ws_client_message(&registry, &instance_id, frame);
                }
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    send_task.abort();

    let mut registry = registry.lock().expect("registry lock poisoned");
    if let Some(instance) = registry.instances.get_mut(&instance_id) {
        instance.ws_tx = None;
    }
}

async fn send_ws_commands(
    sender: &mut SplitSink<WebSocket, Message>,
    rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(command) = rx.recv().await {
        let message = match serde_json::to_vec(&InternalWsServerMessage::Command { data: command })
        {
            Ok(message) => message,
            Err(_) => continue,
        };

        if sender.send(Message::Binary(message.into())).await.is_err() {
            break;
        }
    }
}

fn handle_ws_client_message(
    registry: &SharedRegistry,
    instance_id: &str,
    message: InternalWsClientMessage,
) {
    match message {
        InternalWsClientMessage::Output { data } => {
            let mut registry = registry.lock().expect("registry lock poisoned");
            let Some(instance) = registry.instances.get_mut(instance_id) else {
                return;
            };

            instance.screen.process(&data);
            instance.screen_tail = render_screen_tail(&instance.screen);
            apply_observation(instance);
            instance.updated_at_ms = now_millis();
        }
        InternalWsClientMessage::Resize { rows, cols } => {
            let mut registry = registry.lock().expect("registry lock poisoned");
            let Some(instance) = registry.instances.get_mut(instance_id) else {
                return;
            };

            instance.screen.screen_mut().set_size(rows, cols);
            instance.screen_tail = render_screen_tail(&instance.screen);
            apply_observation(instance);
            instance.updated_at_ms = now_millis();
        }
        InternalWsClientMessage::Focus { focused } => {
            let mut registry = registry.lock().expect("registry lock poisoned");
            let Some(instance) = registry.instances.get_mut(instance_id) else {
                return;
            };

            instance.focused = focused;
            instance.updated_at_ms = now_millis();
        }
    }
}

fn queue_adapter_action(
    registry: &SharedRegistry,
    instance_id: &str,
    build_command: impl FnOnce(&AgentInstance) -> Result<Vec<u8>, AdapterError>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    let mut registry = registry.lock().expect("registry lock poisoned");
    let instance = registry
        .instances
        .get_mut(instance_id)
        .ok_or(ApiError::NotFound)?;

    if instance.status == "exited" {
        return Err(ApiError::ProcessExited);
    }

    let command = build_command(instance)?;
    let adapter = Some(instance.adapter.name().to_string());
    if let Some(ws_tx) = &instance.ws_tx {
        if ws_tx.send(command.clone()).is_err() {
            instance.ws_tx = None;
            instance.command_queue.push_back(command);
        }
    } else {
        instance.command_queue.push_back(command);
    }
    instance.updated_at_ms = now_millis();

    Ok((
        StatusCode::ACCEPTED,
        Json(ActionQueued {
            queued: true,
            adapter,
        }),
    ))
}

fn queue_command(
    registry: &SharedRegistry,
    instance_id: &str,
    command: Vec<u8>,
    adapter: Option<String>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    let mut registry = registry.lock().expect("registry lock poisoned");
    let instance = registry
        .instances
        .get_mut(instance_id)
        .ok_or(ApiError::NotFound)?;

    if instance.status == "exited" {
        return Err(ApiError::ProcessExited);
    }

    if let Some(ws_tx) = &instance.ws_tx {
        if ws_tx.send(command.clone()).is_err() {
            instance.ws_tx = None;
            instance.command_queue.push_back(command);
        }
    } else {
        instance.command_queue.push_back(command);
    }
    instance.updated_at_ms = now_millis();

    Ok((
        StatusCode::ACCEPTED,
        Json(ActionQueued {
            queued: true,
            adapter,
        }),
    ))
}

fn apply_observation(instance: &mut AgentInstance) {
    if instance.status == "exited" {
        return;
    }

    let observation = instance.adapter.observe(instance.screen_tail.as_bytes());
    instance.status = observation.status.as_str().to_string();
    instance.ui_mode = observation.ui_mode.as_str().to_string();
    instance.blocking_reason = observation.blocking_reason;
    instance.current_agent = observation.current_agent;
    instance.current_model = observation.current_model;
    instance.current_provider = observation.current_provider;
    instance.current_reasoning_effort = observation.current_reasoning_effort;
    instance.current_context_window = observation.current_context_window;
    instance.current_context_usage_percent = observation.current_context_usage_percent;
}

impl AgentInstance {
    fn view(&self) -> AgentView {
        AgentView {
            id: self.id.clone(),
            agent_kind: self.agent_kind.clone(),
            adapter: self.adapter.name().to_string(),
            pid: self.pid,
            cwd: self.cwd.clone(),
            command: self.command.clone(),
            status: self.status.clone(),
            ui_mode: self.ui_mode.clone(),
            blocking_reason: self.blocking_reason.clone(),
            current_agent: self.current_agent.clone(),
            current_model: self.current_model.clone(),
            current_provider: self.current_provider.clone(),
            current_reasoning_effort: self.current_reasoning_effort.clone(),
            current_context_window: self.current_context_window.clone(),
            current_context_usage_percent: self.current_context_usage_percent,
            focused: self.focused,
            exit_status: self.exit_status.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            screen_tail: self.screen_tail.clone(),
        }
    }
}

#[derive(Debug, Error)]
enum ApiError {
    #[error("agent instance not found")]
    NotFound,
    #[error("agent instance has exited")]
    ProcessExited,
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::ProcessExited => StatusCode::CONFLICT,
            Self::Adapter(error) => {
                StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::ProcessExited => "process_exited",
            Self::Adapter(error) => error.code(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ErrorBody {
            error: self.code().to_string(),
            message: self.to_string(),
        };

        (status, Json(body)).into_response()
    }
}
