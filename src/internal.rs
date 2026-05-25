use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InternalWsClientMessage {
    Output { data: Vec<u8> },
    Resize { rows: u16, cols: u16 },
    Focus { focused: bool },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InternalWsServerMessage {
    Command { data: Vec<u8> },
}
