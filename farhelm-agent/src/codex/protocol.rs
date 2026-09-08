// SPDX-License-Identifier: Apache-2.0
// Field bindings adapted from OpenAI Codex App Server's generated schemas:
// 0.153.4: 3d2ee51ca2d5db578f328aa75e20aa22c0197c9a
// 0.147.0: be6e8eac029b183056b7e4402879f15d2c85f61b
// Changes: only FarHelm's supported fields; serde-only dependencies; text input.
// See third-party-notices.txt for attribution and the Apache-2.0 license.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub version: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams<'a> {
    pub client_info: ClientInfo<'a>,
    pub capabilities: Capabilities,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub experimental_api: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAccountResponse {
    pub requires_openai_auth: bool,
    pub account: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    pub archived: bool,
    pub limit: u32,
    pub cursor: Option<String>,
    pub source_kinds: [&'static str; 5],
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnsListParams<'a> {
    pub thread_id: &'a str,
    pub cursor: Option<&'a str>,
    pub limit: u32,
    pub items_view: &'static str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams<'a> {
    pub thread_id: &'a str,
    pub include_turns: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPage {
    pub data: Vec<Value>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput<'a> {
    Text { text: &'a str },
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams<'a> {
    pub thread_id: &'a str,
    pub input: [UserInput<'a>; 1],
    pub client_user_message_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'a str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams<'a> {
    pub thread_id: &'a str,
    pub expected_turn_id: &'a str,
    pub input: [UserInput<'a>; 1],
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams<'a> {
    pub thread_id: &'a str,
    pub turn_id: &'a str,
}
