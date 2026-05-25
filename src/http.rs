use crate::api::{AgentView, ExitRequest, HealthResponse, RegisterAgentRequest};
use crate::internal::{InternalWsClientMessage, InternalWsServerMessage};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use reqwest::blocking::Client;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use tokio::runtime::Builder;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone)]
pub struct ControlClient {
    addr: String,
    client: Client,
}

pub struct InternalWsHandle {
    command_rx: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    output_tx: tokio_mpsc::UnboundedSender<InternalWsClientMessage>,
}

impl ControlClient {
    pub fn new(addr: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            addr: addr.to_string(),
            client,
        })
    }

    pub fn is_healthy(&self) -> bool {
        self.client
            .get(self.url("/health"))
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<HealthResponse>())
            .map(|health| health.ok)
            .unwrap_or(false)
    }

    pub fn register_agent(&self, request: &RegisterAgentRequest) -> Result<AgentView> {
        let response = self
            .client
            .post(self.url("/internal/agents/register"))
            .json(request)
            .send()
            .context("failed to send register request")?;

        parse_json_response(response).context("daemon rejected instance registration")
    }

    pub fn post_exit(&self, instance_id: &str, exit: &ExitRequest) -> Result<()> {
        let response = self
            .client
            .post(self.url(&format!("/internal/agents/{instance_id}/exit")))
            .json(exit)
            .send()
            .context("failed to post exit status")?;

        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            Ok(())
        } else {
            bail!(
                "expected HTTP 204 No Content, got HTTP {status}: {}",
                response_text(response)
            )
        }
    }

    pub fn connect_agent_ws(&self, instance_id: &str) -> Result<InternalWsHandle> {
        let url = format!("ws://{}/internal/agents/{instance_id}/ws", self.addr);
        let (command_tx, command_rx) = mpsc::channel::<Vec<u8>>();
        let (output_tx, mut output_rx) = tokio_mpsc::unbounded_channel::<InternalWsClientMessage>();

        thread::spawn(move || {
            let runtime = match Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(_) => return,
            };

            runtime.block_on(async move {
                let Ok((stream, _)) = connect_async(&url).await else {
                    return;
                };
                let (mut write, mut read) = stream.split();

                let write_task = tokio::spawn(async move {
                    while let Some(output) = output_rx.recv().await {
                        let Ok(message) = serde_json::to_vec(&output) else {
                            continue;
                        };

                        if write.send(Message::Binary(message.into())).await.is_err() {
                            break;
                        }
                    }
                });

                while let Some(message) = read.next().await {
                    let Ok(message) = message else {
                        break;
                    };

                    let payload = match message {
                        Message::Text(text) => {
                            serde_json::from_str::<InternalWsServerMessage>(&text).ok()
                        }
                        Message::Binary(binary) => {
                            serde_json::from_slice::<InternalWsServerMessage>(&binary).ok()
                        }
                        Message::Close(_) => break,
                        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => None,
                    };

                    if let Some(InternalWsServerMessage::Command { data }) = payload {
                        if command_tx.send(data).is_err() {
                            break;
                        }
                    }
                }

                write_task.abort();
            });
        });

        Ok(InternalWsHandle {
            command_rx: Arc::new(Mutex::new(command_rx)),
            output_tx,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
}

impl InternalWsHandle {
    pub fn recv_command_timeout(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        self.command_rx
            .lock()
            .ok()
            .and_then(|rx| rx.recv_timeout(timeout).ok())
    }

    pub fn send_output(&self, output: &[u8]) -> Result<()> {
        self.output_tx
            .send(InternalWsClientMessage::Output {
                data: output.to_vec(),
            })
            .map_err(|_| anyhow::anyhow!("internal websocket disconnected"))
    }

    pub fn send_resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.output_tx
            .send(InternalWsClientMessage::Resize { rows, cols })
            .map_err(|_| anyhow::anyhow!("internal websocket disconnected"))
    }

    pub fn send_focus(&self, focused: bool) -> Result<()> {
        self.output_tx
            .send(InternalWsClientMessage::Focus { focused })
            .map_err(|_| anyhow::anyhow!("internal websocket disconnected"))
    }
}

impl Clone for InternalWsHandle {
    fn clone(&self) -> Self {
        Self {
            command_rx: Arc::clone(&self.command_rx),
            output_tx: self.output_tx.clone(),
        }
    }
}

fn parse_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::blocking::Response,
) -> Result<T> {
    let status = response.status();
    if status.is_success() {
        return response
            .json::<T>()
            .context("failed to decode JSON response");
    }

    bail!("HTTP {status}: {}", response_text(response))
}

fn response_text(response: reqwest::blocking::Response) -> String {
    response
        .text()
        .unwrap_or_else(|_| "<unreadable body>".to_string())
}
