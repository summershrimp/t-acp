use crate::adapters::{self, Adapter, AdapterError};
use crate::api::{
    ActionQueued, AgentEventState, AgentList, AgentStreamEvent, AgentView, CursorPosition,
    ErrorBody, ExitRequest, HealthResponse, InteractionRequest, ObservationEvent, ObservationView,
    RegisterAgentRequest, ScreenSnapshot, SubmitInteractionRequest,
};
use crate::interactions::{InteractionSubmission, validate_submission};
use crate::internal::{InternalWsClientMessage, InternalWsServerMessage};
use crate::util::now_millis;
use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::stream::{self, SplitSink};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

type SharedRegistry = Arc<Mutex<Registry>>;
const INITIAL_EVENT_SEQ: u64 = 1;
const MAX_RAW_TAIL_BYTES: usize = 128 * 1024;
const MAX_OBSERVATION_EVENTS: usize = 256;
const DEFAULT_SCREEN_ROWS: u16 = 24;
const DEFAULT_SCREEN_COLS: u16 = 80;
const MAX_SCREEN_ROWS: u16 = 200;
const MAX_SCREEN_COLS: u16 = 500;

#[derive(Default)]
struct Registry {
    instances: HashMap<String, AgentInstance>,
    global_event_subscribers: Vec<mpsc::UnboundedSender<AgentStreamEvent>>,
}

fn lock_registry(registry: &SharedRegistry) -> std::sync::MutexGuard<'_, Registry> {
    match registry.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            warn!("registry lock was poisoned; recovering in-memory registry state");
            registry.clear_poison();
            poisoned.into_inner()
        }
    }
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
    need_interactive: bool,
    interactive_kind: Option<String>,
    interaction_request: Option<InteractionRequest>,
    focused: bool,
    exit_status: Option<String>,
    screen: vt100::Parser,
    screen_tail: String,
    raw_tail: VecDeque<u8>,
    observation_events: VecDeque<ObservationEvent>,
    next_observation_event_seq: u64,
    command_queue: VecDeque<Vec<u8>>,
    ws_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    next_event_seq: u64,
    event_subscribers: Vec<mpsc::UnboundedSender<AgentStreamEvent>>,
    created_at_ms: u128,
    updated_at_ms: u128,
}

#[derive(Clone, Copy)]
enum TrackedEventKind {
    StateChanged,
    Exited,
}

pub async fn run(addr: &str) -> Result<()> {
    let registry = Arc::new(Mutex::new(Registry::default()));
    let app = Router::new()
        .route("/health", get(health))
        .route("/observe", get(observe_dashboard_html))
        .route("/agents", get(list_agents))
        .route("/agents/events/stream", get(stream_all_agent_events))
        .route("/agents/{instance_id}", get(get_agent).delete(delete_agent))
        .route("/agents/{instance_id}/observe", get(observe_agent_html))
        .route("/agents/{instance_id}/observations", get(get_observations))
        .route(
            "/agents/{instance_id}/events/stream",
            get(stream_agent_events),
        )
        .route("/agents/{instance_id}/input", post(post_input))
        .route(
            "/agents/{instance_id}/interaction",
            post(submit_interaction),
        )
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
    let registry = lock_registry(&registry);
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
    let registry = lock_registry(&registry);
    let instance = registry
        .instances
        .get(&instance_id)
        .ok_or(ApiError::NotFound)?;

    Ok(Json(instance.view()))
}

async fn get_observations(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<Json<ObservationView>, ApiError> {
    let registry = lock_registry(&registry);
    let instance = registry
        .instances
        .get(&instance_id)
        .ok_or(ApiError::NotFound)?;

    Ok(Json(instance.observation_view()))
}

async fn observe_agent_html(Path(instance_id): Path<String>) -> Html<String> {
    Html(observe_html(Some(&instance_id)))
}

async fn observe_dashboard_html() -> Html<String> {
    Html(observe_html(None))
}

async fn stream_agent_events(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let (snapshot, rx) = {
        let mut registry = lock_registry(&registry);
        let instance = registry
            .instances
            .get_mut(&instance_id)
            .ok_or(ApiError::NotFound)?;
        let snapshot = instance.snapshot_event();
        let (tx, rx) = mpsc::unbounded_channel();
        instance.event_subscribers.push(tx);
        (snapshot, rx)
    };

    let stream = stream::once(async move { Ok(event_to_sse(snapshot)) })
        .chain(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (Ok(event_to_sse(event)), rx))
        }));

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn stream_all_agent_events(
    State(registry): State<SharedRegistry>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = {
        let mut registry = lock_registry(&registry);
        let (tx, rx) = mpsc::unbounded_channel();
        registry.global_event_subscribers.push(tx);
        rx
    };

    let stream = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| (Ok(event_to_sse(event)), rx))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn post_input(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_command(&registry, &instance_id, body.to_vec(), None)
}

async fn submit_interaction(
    State(registry): State<SharedRegistry>,
    Path(instance_id): Path<String>,
    Json(request): Json<SubmitInteractionRequest>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    queue_interaction_action(&registry, &instance_id, request)
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
    let (screen_rows, screen_cols) = sanitized_screen_size(Some(request.rows), Some(request.cols));
    let mut instance = AgentInstance {
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
        need_interactive: observation.need_interactive,
        interactive_kind: observation.interactive_kind,
        interaction_request: observation.interaction_request,
        focused: false,
        exit_status: None,
        screen: vt100::Parser::new(screen_rows, screen_cols, 2000),
        screen_tail: String::new(),
        raw_tail: VecDeque::new(),
        observation_events: VecDeque::new(),
        next_observation_event_seq: 0,
        command_queue: VecDeque::new(),
        ws_tx: None,
        next_event_seq: INITIAL_EVENT_SEQ,
        event_subscribers: Vec::new(),
        created_at_ms: now,
        updated_at_ms: now,
    };
    instance.push_observation_event(
        "instance_registered",
        format!("registered {}", instance.command),
    );
    let view = instance.view();

    lock_registry(&registry)
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
        let registry = lock_registry(&registry);
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
    let mut registry = lock_registry(&registry);
    let instance = registry
        .instances
        .get_mut(&instance_id)
        .ok_or(ApiError::NotFound)?;
    let previous_state = instance.event_state();

    instance.status = "exited".to_string();
    instance.ui_mode = "unknown".to_string();
    instance.blocking_reason = None;
    instance.need_interactive = false;
    instance.interactive_kind = None;
    instance.interaction_request = None;
    instance.exit_status = Some(request.status);
    instance.ws_tx = None;
    instance.push_observation_event(
        "instance_exited",
        format!(
            "exit status {}",
            instance.exit_status.as_deref().unwrap_or("unknown")
        ),
    );
    instance.updated_at_ms = now_millis();
    let event = publish_if_changed(instance, previous_state, TrackedEventKind::Exited);
    if let Some(event) = event {
        publish_global_event(&mut registry, event);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_agent_ws(registry: SharedRegistry, instance_id: String, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    {
        let mut registry = lock_registry(&registry);
        let Some(instance) = registry.instances.get_mut(&instance_id) else {
            return;
        };
        instance.ws_tx = Some(tx);

        let mut flushed = 0_usize;
        while let Some(command) = instance.command_queue.pop_front() {
            let Some(ws_tx) = &instance.ws_tx else {
                instance.command_queue.push_front(command);
                break;
            };

            match ws_tx.send(command) {
                Ok(()) => flushed += 1,
                Err(error) => {
                    instance.ws_tx = None;
                    instance.command_queue.push_front(error.0);
                    break;
                }
            }
        }
        if flushed > 0 {
            instance.push_observation_event(
                "queued_input_dequeued",
                format!("flushed {flushed} command(s)"),
            );
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

    let mut registry = lock_registry(&registry);
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
    let mut registry = lock_registry(registry);
    let Some(instance) = registry.instances.get_mut(instance_id) else {
        return;
    };
    let previous_state = instance.event_state();

    match message {
        InternalWsClientMessage::Output { data } => {
            append_output_bytes(instance, &data);
        }
        InternalWsClientMessage::Resize { rows, cols } => {
            let previous_status = instance.status.clone();
            let previous_ui_mode = instance.ui_mode.clone();
            let previous_interaction_id = current_interaction_id(instance);
            let (rows, cols) = sanitized_screen_size(Some(rows), Some(cols));

            resize_screen(instance, rows, cols);
            apply_observation(instance);
            push_observation_events(
                instance,
                previous_status,
                previous_ui_mode,
                previous_interaction_id,
            );
            instance.push_observation_event("screen_resized", format!("{rows}x{cols}"));
            instance.push_observation_event("screen_updated", screen_summary(instance));
            instance.updated_at_ms = now_millis();
        }
        InternalWsClientMessage::Focus { focused } => {
            instance.focused = focused;
            instance.push_observation_event(
                "focus_changed",
                if focused { "focused" } else { "blurred" },
            );
            instance.updated_at_ms = now_millis();
        }
    }

    let event = publish_if_changed(instance, previous_state, TrackedEventKind::StateChanged);
    if let Some(event) = event {
        publish_global_event(&mut registry, event);
    }
}

fn append_output_bytes(instance: &mut AgentInstance, bytes: &[u8]) {
    append_raw_tail(&mut instance.raw_tail, bytes);
    instance.push_observation_event("pty_output_received", format!("{} bytes", bytes.len()));

    let previous_status = instance.status.clone();
    let previous_ui_mode = instance.ui_mode.clone();
    let previous_interaction_id = current_interaction_id(instance);

    grow_screen_for_output(instance, bytes);
    instance.screen.process(bytes);
    instance.screen_tail = render_screen_tail(&instance.screen);
    apply_observation(instance);
    push_observation_events(
        instance,
        previous_status,
        previous_ui_mode,
        previous_interaction_id,
    );
    instance.push_observation_event("screen_updated", screen_summary(instance));
    instance.updated_at_ms = now_millis();
}

fn current_interaction_id(instance: &AgentInstance) -> Option<String> {
    instance
        .interaction_request
        .as_ref()
        .map(|interaction| interaction.id.clone())
}

fn push_observation_events(
    instance: &mut AgentInstance,
    previous_status: String,
    previous_ui_mode: String,
    previous_interaction_id: Option<String>,
) {
    if instance.status != previous_status || instance.ui_mode != previous_ui_mode {
        instance.push_observation_event(
            "state_changed",
            format!(
                "{}:{} -> {}:{}",
                previous_status, previous_ui_mode, instance.status, instance.ui_mode
            ),
        );
    }

    if current_interaction_id(instance) != previous_interaction_id {
        let event = instance
            .interaction_request
            .as_ref()
            .map(|interaction| {
                (
                    "interaction_detected",
                    format!(
                        "{}: {}",
                        interaction.kind,
                        interaction
                            .prompt
                            .clone()
                            .or_else(|| interaction.title.clone())
                            .unwrap_or_else(|| "interaction request detected".to_string())
                    ),
                )
            })
            .unwrap_or((
                "interaction_cleared",
                "interaction request cleared".to_string(),
            ));
        instance.push_observation_event(event.0, event.1);
    }
}

fn queue_adapter_action(
    registry: &SharedRegistry,
    instance_id: &str,
    build_command: impl FnOnce(&AgentInstance) -> Result<Vec<u8>, AdapterError>,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    let mut registry = lock_registry(registry);
    let instance = registry
        .instances
        .get_mut(instance_id)
        .ok_or(ApiError::NotFound)?;

    if instance.status == "exited" {
        return Err(ApiError::ProcessExited);
    }

    let command = build_command(instance)?;
    let adapter = Some(instance.adapter.name().to_string());
    let command_len = command.len();
    let delivered = dispatch_command(instance, command);
    instance.push_observation_event(
        "adapter_action_queued",
        format!(
            "{} {} {} bytes",
            adapter.as_deref().unwrap_or("unknown"),
            if delivered { "sent" } else { "queued" },
            command_len
        ),
    );
    instance.updated_at_ms = now_millis();

    Ok((
        StatusCode::ACCEPTED,
        Json(ActionQueued {
            queued: true,
            adapter,
        }),
    ))
}

fn queue_interaction_action(
    registry: &SharedRegistry,
    instance_id: &str,
    request: SubmitInteractionRequest,
) -> Result<(StatusCode, Json<ActionQueued>), ApiError> {
    let mut registry = lock_registry(registry);
    let instance = registry
        .instances
        .get_mut(instance_id)
        .ok_or(ApiError::NotFound)?;

    if instance.status == "exited" {
        return Err(ApiError::ProcessExited);
    }

    let submission =
        validate_submission(&request).map_err(|error| ApiError::BadRequest(error.to_string()))?;

    let interaction = instance
        .interaction_request
        .clone()
        .ok_or(ApiError::StaleInteraction)?;
    if interaction.id != request.interaction_id {
        return Err(ApiError::StaleInteraction);
    }
    if matches!(submission, InteractionSubmission::CustomAnswer { .. })
        && !interaction.custom_answer_allowed
    {
        return Err(ApiError::BadRequest(
            "custom answer is not allowed for this interaction".to_string(),
        ));
    }

    let command = instance
        .adapter
        .submit_interaction(&interaction, &submission)?;
    let adapter = Some(instance.adapter.name().to_string());
    let command_len = command.len();
    let delivered = dispatch_command(instance, command);
    instance.push_observation_event(
        "interaction_action_queued",
        format!(
            "{} {} {} {} bytes",
            interaction.kind,
            submission.label(),
            if delivered { "sent" } else { "queued" },
            command_len
        ),
    );
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
    let mut registry = lock_registry(registry);
    let instance = registry
        .instances
        .get_mut(instance_id)
        .ok_or(ApiError::NotFound)?;

    if instance.status == "exited" {
        return Err(ApiError::ProcessExited);
    }

    let command_len = command.len();
    let delivered = dispatch_command(instance, command);
    instance.push_observation_event(
        "raw_input_queued",
        format!(
            "{} {} bytes",
            if delivered { "sent" } else { "queued" },
            command_len
        ),
    );
    instance.updated_at_ms = now_millis();

    Ok((
        StatusCode::ACCEPTED,
        Json(ActionQueued {
            queued: true,
            adapter,
        }),
    ))
}

fn dispatch_command(instance: &mut AgentInstance, command: Vec<u8>) -> bool {
    if let Some(ws_tx) = &instance.ws_tx {
        match ws_tx.send(command) {
            Ok(()) => return true,
            Err(error) => {
                instance.ws_tx = None;
                instance.command_queue.push_back(error.0);
                return false;
            }
        }
    }

    instance.command_queue.push_back(command);
    false
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
    instance.need_interactive = observation.need_interactive;
    instance.interactive_kind = observation.interactive_kind;
    instance.interaction_request = observation.interaction_request;
}

impl AgentInstance {
    fn event_state(&self) -> AgentEventState {
        AgentEventState {
            status: self.status.clone(),
            ui_mode: self.ui_mode.clone(),
            blocking_reason: self.blocking_reason.clone(),
            current_agent: self.current_agent.clone(),
            current_model: self.current_model.clone(),
            current_provider: self.current_provider.clone(),
            current_reasoning_effort: self.current_reasoning_effort.clone(),
            current_context_window: self.current_context_window.clone(),
            current_context_usage_percent: self.current_context_usage_percent,
            need_interactive: self.need_interactive,
            interactive_kind: self.interactive_kind.clone(),
            focused: self.focused,
            exit_status: self.exit_status.clone(),
        }
    }

    fn snapshot_event(&self) -> AgentStreamEvent {
        AgentStreamEvent::Snapshot {
            seq: self.next_event_seq.saturating_sub(1),
            instance_id: self.id.clone(),
            ts_ms: now_millis(),
            state: self.event_state(),
        }
    }

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
            need_interactive: self.need_interactive,
            interactive_kind: self.interactive_kind.clone(),
            interaction_request: self.interaction_request.clone(),
            focused: self.focused,
            exit_status: self.exit_status.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            screen_tail: self.screen_tail.clone(),
        }
    }

    fn observation_view(&self) -> ObservationView {
        let raw_tail = self.raw_tail.iter().copied().collect::<Vec<_>>();
        ObservationView {
            agent: self.view(),
            screen: self.screen_snapshot(),
            events: self.observation_events.iter().cloned().collect(),
            raw_tail_hex: bytes_to_hex(&raw_tail),
            raw_tail_utf8_lossy: String::from_utf8_lossy(&raw_tail).to_string(),
        }
    }

    fn screen_snapshot(&self) -> ScreenSnapshot {
        let screen = self.screen.screen();
        let (rows, cols) = screen.size();
        let (row, col) = screen.cursor_position();
        let text = screen.contents();
        let lines = text.lines().map(str::to_string).collect();

        ScreenSnapshot {
            rows,
            cols,
            cursor: CursorPosition { row, col },
            lines,
            text,
            updated_at_ms: self.updated_at_ms,
        }
    }

    fn push_observation_event(&mut self, kind: impl Into<String>, message: impl Into<String>) {
        let event = ObservationEvent {
            seq: self.next_observation_event_seq,
            at_ms: now_millis(),
            kind: kind.into(),
            message: message.into(),
        };
        self.next_observation_event_seq = self.next_observation_event_seq.saturating_add(1);
        self.observation_events.push_back(event);
        while self.observation_events.len() > MAX_OBSERVATION_EVENTS {
            self.observation_events.pop_front();
        }
    }
}

fn publish_if_changed(
    instance: &mut AgentInstance,
    previous_state: AgentEventState,
    event_kind: TrackedEventKind,
) -> Option<AgentStreamEvent> {
    let current_state = instance.event_state();
    let changed_fields = diff_event_state(&previous_state, &current_state);
    if changed_fields.is_empty() {
        return None;
    }

    let event = match event_kind {
        TrackedEventKind::StateChanged => AgentStreamEvent::StateChanged {
            seq: instance.next_event_seq,
            instance_id: instance.id.clone(),
            ts_ms: now_millis(),
            changed_fields,
            state: current_state,
        },
        TrackedEventKind::Exited => AgentStreamEvent::Exited {
            seq: instance.next_event_seq,
            instance_id: instance.id.clone(),
            ts_ms: now_millis(),
            changed_fields,
            state: current_state,
        },
    };

    instance.next_event_seq += 1;
    instance
        .event_subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    Some(event)
}

fn publish_global_event(registry: &mut Registry, event: AgentStreamEvent) {
    registry
        .global_event_subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

fn diff_event_state(previous: &AgentEventState, current: &AgentEventState) -> Vec<String> {
    let mut changed_fields = Vec::new();

    if previous.status != current.status {
        changed_fields.push("status".to_string());
    }
    if previous.ui_mode != current.ui_mode {
        changed_fields.push("ui_mode".to_string());
    }
    if previous.blocking_reason != current.blocking_reason {
        changed_fields.push("blocking_reason".to_string());
    }
    if previous.current_agent != current.current_agent {
        changed_fields.push("current_agent".to_string());
    }
    if previous.current_model != current.current_model {
        changed_fields.push("current_model".to_string());
    }
    if previous.current_provider != current.current_provider {
        changed_fields.push("current_provider".to_string());
    }
    if previous.current_reasoning_effort != current.current_reasoning_effort {
        changed_fields.push("current_reasoning_effort".to_string());
    }
    if previous.current_context_window != current.current_context_window {
        changed_fields.push("current_context_window".to_string());
    }
    if previous.current_context_usage_percent != current.current_context_usage_percent {
        changed_fields.push("current_context_usage_percent".to_string());
    }
    if previous.need_interactive != current.need_interactive {
        changed_fields.push("need_interactive".to_string());
    }
    if previous.interactive_kind != current.interactive_kind {
        changed_fields.push("interactive_kind".to_string());
    }
    if previous.focused != current.focused {
        changed_fields.push("focused".to_string());
    }
    if previous.exit_status != current.exit_status {
        changed_fields.push("exit_status".to_string());
    }

    changed_fields
}

fn event_to_sse(event: AgentStreamEvent) -> Event {
    let seq = event.event_id();
    let event_name = event.event_name();
    let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());

    Event::default().id(seq).event(event_name).data(data)
}

fn append_raw_tail(raw_tail: &mut VecDeque<u8>, bytes: &[u8]) {
    raw_tail.extend(bytes.iter().copied());
    while raw_tail.len() > MAX_RAW_TAIL_BYTES {
        raw_tail.pop_front();
    }
}

fn grow_screen_for_output(instance: &mut AgentInstance, bytes: &[u8]) {
    let Some((hint_rows, hint_cols)) = cursor_position_hint(bytes) else {
        return;
    };

    let (rows, cols) = instance.screen.screen().size();
    let rows = rows.max(hint_rows).min(MAX_SCREEN_ROWS);
    let cols = cols.max(hint_cols).min(MAX_SCREEN_COLS);
    resize_screen(instance, rows, cols);
}

fn resize_screen(instance: &mut AgentInstance, rows: u16, cols: u16) {
    let (current_rows, current_cols) = instance.screen.screen().size();
    if current_rows == rows && current_cols == cols {
        return;
    }

    instance.screen.screen_mut().set_size(rows, cols);
    instance.screen_tail = render_screen_tail(&instance.screen);
}

fn sanitized_screen_size(rows: Option<u16>, cols: Option<u16>) -> (u16, u16) {
    (
        rows.unwrap_or(DEFAULT_SCREEN_ROWS)
            .clamp(1, MAX_SCREEN_ROWS),
        cols.unwrap_or(DEFAULT_SCREEN_COLS)
            .clamp(1, MAX_SCREEN_COLS),
    )
}

fn cursor_position_hint(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut idx = 0;
    let mut rows = 0_u16;
    let mut cols = 0_u16;

    while idx + 2 < bytes.len() {
        if bytes[idx] != 0x1b || bytes[idx + 1] != b'[' {
            idx += 1;
            continue;
        }

        let params_start = idx + 2;
        let mut final_idx = params_start;
        while final_idx < bytes.len() && !(b'@'..=b'~').contains(&bytes[final_idx]) {
            final_idx += 1;
        }
        if final_idx >= bytes.len() {
            break;
        }

        if matches!(bytes[final_idx], b'H' | b'f')
            && let Some((row, col)) = parse_cursor_position_params(&bytes[params_start..final_idx])
        {
            rows = rows.max(row);
            cols = cols.max(col);
        }

        idx = final_idx + 1;
    }

    if rows > 0 || cols > 0 {
        Some((rows.max(1), cols.max(1)))
    } else {
        None
    }
}

fn parse_cursor_position_params(params: &[u8]) -> Option<(u16, u16)> {
    if params
        .iter()
        .any(|byte| !byte.is_ascii_digit() && *byte != b';')
    {
        return None;
    }

    let params = std::str::from_utf8(params).ok()?;
    let mut parts = params.split(';');
    let row = parse_cursor_position_part(parts.next(), 1)?;
    let col = parse_cursor_position_part(parts.next(), 1)?;

    Some((row, col))
}

fn parse_cursor_position_part(part: Option<&str>, default: u16) -> Option<u16> {
    let part = part.unwrap_or_default();
    if part.is_empty() {
        Some(default)
    } else {
        part.parse::<u16>().ok().map(|value| value.max(1))
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 3);
    for (idx, byte) in bytes.iter().copied().enumerate() {
        if idx > 0 {
            output.push(' ');
        }
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn screen_summary(instance: &AgentInstance) -> String {
    let (rows, cols) = instance.screen.screen().size();
    format!("{rows}x{cols} {}", instance.ui_mode)
}

fn observe_html(instance_id: Option<&str>) -> String {
    let agent_id_json = serde_json::to_string(&instance_id).unwrap_or_else(|_| "null".to_string());
    OBSERVE_HTML.replace("__INITIAL_AGENT_ID__", &agent_id_json)
}

const OBSERVE_HTML: &str = include_str!("../assets/observe.html");

#[derive(Debug, Error)]
enum ApiError {
    #[error("agent instance not found")]
    NotFound,
    #[error("agent instance has exited")]
    ProcessExited,
    #[error("{0}")]
    BadRequest(String),
    #[error("interaction request is stale or no longer visible")]
    StaleInteraction,
    #[error(transparent)]
    Adapter(#[from] AdapterError),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::ProcessExited => StatusCode::CONFLICT,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::StaleInteraction => StatusCode::CONFLICT,
            Self::Adapter(error) => {
                StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::ProcessExited => "process_exited",
            Self::BadRequest(_) => "bad_request",
            Self::StaleInteraction => "stale_interaction",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{AdapterObservation, InstanceStatus, UiMode};

    fn sample_instance() -> AgentInstance {
        AgentInstance {
            id: "instance-1".to_string(),
            agent_kind: "opencode".to_string(),
            adapter: adapters::for_agent_kind("opencode"),
            pid: Some(42),
            cwd: "/tmp".to_string(),
            command: "opencode".to_string(),
            status: "ready".to_string(),
            ui_mode: "input".to_string(),
            blocking_reason: None,
            current_agent: Some("Build".to_string()),
            current_model: Some("gpt-5.4".to_string()),
            current_provider: Some("GitHub Copilot".to_string()),
            current_reasoning_effort: Some("high".to_string()),
            current_context_window: Some("42.6K".to_string()),
            current_context_usage_percent: Some(21),
            need_interactive: false,
            interactive_kind: None,
            interaction_request: None,
            focused: true,
            exit_status: None,
            screen: vt100::Parser::new(24, 80, 2000),
            screen_tail: String::new(),
            raw_tail: VecDeque::new(),
            observation_events: VecDeque::new(),
            next_observation_event_seq: 0,
            command_queue: VecDeque::new(),
            ws_tx: None,
            next_event_seq: INITIAL_EVENT_SEQ,
            event_subscribers: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn diff_event_state_reports_only_tracked_changes() {
        let previous = AgentEventState {
            status: "ready".to_string(),
            ui_mode: "input".to_string(),
            blocking_reason: None,
            current_agent: Some("Build".to_string()),
            current_model: Some("gpt-5.4".to_string()),
            current_provider: Some("GitHub Copilot".to_string()),
            current_reasoning_effort: Some("high".to_string()),
            current_context_window: Some("42.6K".to_string()),
            current_context_usage_percent: Some(21),
            need_interactive: false,
            interactive_kind: None,
            focused: true,
            exit_status: None,
        };
        let current = AgentEventState {
            status: "blocked".to_string(),
            ui_mode: "permission_prompt".to_string(),
            blocking_reason: Some("permission".to_string()),
            current_agent: Some("Build".to_string()),
            current_model: Some("gpt-5.4".to_string()),
            current_provider: Some("GitHub Copilot".to_string()),
            current_reasoning_effort: Some("high".to_string()),
            current_context_window: Some("42.6K".to_string()),
            current_context_usage_percent: Some(21),
            need_interactive: true,
            interactive_kind: Some("permission".to_string()),
            focused: false,
            exit_status: None,
        };

        let changed_fields = diff_event_state(&previous, &current);

        assert_eq!(
            changed_fields,
            vec![
                "status",
                "ui_mode",
                "blocking_reason",
                "need_interactive",
                "interactive_kind",
                "focused"
            ]
        );
    }

    #[test]
    fn publish_if_changed_emits_state_change_event() {
        let mut instance = sample_instance();
        let previous_state = instance.event_state();
        let (tx, mut rx) = mpsc::unbounded_channel();
        instance.event_subscribers.push(tx);
        let observation = AdapterObservation {
            status: InstanceStatus::Blocked,
            ui_mode: UiMode::PermissionPrompt,
            blocking_reason: Some("permission".to_string()),
            current_agent: Some("Build".to_string()),
            current_model: Some("gpt-5.4".to_string()),
            current_provider: Some("GitHub Copilot".to_string()),
            current_reasoning_effort: Some("high".to_string()),
            current_context_window: Some("42.6K".to_string()),
            current_context_usage_percent: Some(21),
            need_interactive: true,
            interactive_kind: Some("permission".to_string()),
            interaction_request: None,
        };

        instance.status = observation.status.as_str().to_string();
        instance.ui_mode = observation.ui_mode.as_str().to_string();
        instance.blocking_reason = observation.blocking_reason;
        instance.need_interactive = observation.need_interactive;
        instance.interactive_kind = observation.interactive_kind;
        let event = publish_if_changed(
            &mut instance,
            previous_state,
            TrackedEventKind::StateChanged,
        );
        assert!(event.is_some());

        match rx.try_recv() {
            Ok(AgentStreamEvent::StateChanged {
                seq,
                changed_fields,
                state,
                ..
            }) => {
                assert_eq!(seq, INITIAL_EVENT_SEQ);
                assert_eq!(state.status, "blocked");
                assert_eq!(state.ui_mode, "permission_prompt");
                assert!(state.need_interactive);
                assert_eq!(state.interactive_kind.as_deref(), Some("permission"));
                assert_eq!(
                    changed_fields,
                    vec![
                        "status",
                        "ui_mode",
                        "blocking_reason",
                        "need_interactive",
                        "interactive_kind"
                    ]
                );
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn publish_global_event_fans_out_to_global_subscribers() {
        let mut registry = Registry::default();
        let mut instance = sample_instance();
        let previous_state = instance.event_state();
        let (tx, mut rx) = mpsc::unbounded_channel();
        registry.global_event_subscribers.push(tx);

        instance.focused = false;
        let event = publish_if_changed(
            &mut instance,
            previous_state,
            TrackedEventKind::StateChanged,
        )
        .expect("event should be emitted");
        publish_global_event(&mut registry, event);

        match rx.try_recv() {
            Ok(AgentStreamEvent::StateChanged {
                instance_id,
                changed_fields,
                ..
            }) => {
                assert_eq!(instance_id, "instance-1");
                assert_eq!(changed_fields, vec!["focused"]);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn sse_event_id_includes_instance_and_sequence() {
        let event = AgentStreamEvent::StateChanged {
            seq: 7,
            instance_id: "instance-1".to_string(),
            ts_ms: 99,
            changed_fields: vec!["focused".to_string()],
            state: AgentEventState {
                status: "ready".to_string(),
                ui_mode: "input".to_string(),
                blocking_reason: None,
                current_agent: None,
                current_model: None,
                current_provider: None,
                current_reasoning_effort: None,
                current_context_window: None,
                current_context_usage_percent: None,
                need_interactive: false,
                interactive_kind: None,
                focused: true,
                exit_status: None,
            },
        };

        let sse = event_to_sse(event);
        let rendered = format!("{sse:?}");
        assert!(rendered.contains("instance-1:7"));
    }

    #[test]
    fn view_exposes_interactive_fields() {
        let mut instance = sample_instance();
        instance.need_interactive = true;
        instance.interactive_kind = Some("permission".to_string());

        let view = instance.view();

        assert!(view.need_interactive);
        assert_eq!(view.interactive_kind.as_deref(), Some("permission"));
    }

    #[test]
    fn cursor_position_hint_tracks_absolute_csi_moves() {
        let bytes = b"hello\x1b[39;8HPermission required\x1b[47;165Hconfirm";

        assert_eq!(cursor_position_hint(bytes), Some((47, 165)));
    }

    #[test]
    fn cursor_position_hint_ignores_private_modes() {
        let bytes = b"\x1b[?2026h\x1b[?25l\x1b[12;34H";

        assert_eq!(cursor_position_hint(bytes), Some((12, 34)));
    }

    #[test]
    fn sanitized_screen_size_defaults_and_clamps() {
        assert_eq!(
            sanitized_screen_size(None, None),
            (DEFAULT_SCREEN_ROWS, DEFAULT_SCREEN_COLS)
        );
        assert_eq!(
            sanitized_screen_size(Some(0), Some(900)),
            (1, MAX_SCREEN_COLS)
        );
    }
}
