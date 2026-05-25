use crate::api::{AgentView, ExitRequest, HealthResponse, RegisterAgentRequest};
use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use std::time::Duration;

#[derive(Clone)]
pub struct ControlClient {
    addr: String,
    client: Client,
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

    pub fn post_output(&self, instance_id: &str, output: &[u8]) -> Result<()> {
        let response = self
            .client
            .post(self.url(&format!("/internal/agents/{instance_id}/output")))
            .body(output.to_vec())
            .send()
            .context("failed to post PTY output")?;

        expect_status(response, StatusCode::NO_CONTENT)
    }

    pub fn post_exit(&self, instance_id: &str, exit: &ExitRequest) -> Result<()> {
        let response = self
            .client
            .post(self.url(&format!("/internal/agents/{instance_id}/exit")))
            .json(exit)
            .send()
            .context("failed to post exit status")?;

        expect_status(response, StatusCode::NO_CONTENT)
    }

    pub fn pop_command(&self, instance_id: &str) -> Result<Option<Vec<u8>>> {
        let response = self
            .client
            .get(self.url(&format!("/internal/agents/{instance_id}/commands")))
            .send()
            .context("failed to poll command queue")?;

        match response.status() {
            StatusCode::OK => Ok(Some(
                response
                    .bytes()
                    .context("failed to read command body")?
                    .to_vec(),
            )),
            StatusCode::NO_CONTENT => Ok(None),
            status => bail!(
                "unexpected command poll status {status}: {}",
                response_text(response)
            ),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
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

fn expect_status(response: reqwest::blocking::Response, expected: StatusCode) -> Result<()> {
    let status = response.status();
    if status == expected {
        Ok(())
    } else {
        bail!(
            "expected HTTP {expected}, got HTTP {status}: {}",
            response_text(response)
        )
    }
}

fn response_text(response: reqwest::blocking::Response) -> String {
    response
        .text()
        .unwrap_or_else(|_| "<unreadable body>".to_string())
}
