//! Shared runtime state for the Slack service.

use sdk_rust::OutboundEvent;
use slack_morphism::prelude::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

const BACKEND: &str = "slack-morphism";

/// State shared between Socket Mode callbacks and MCP-lite tool handlers.
/// Always accessed through `Arc<SlackState>`.
#[derive(Debug)]
pub struct SlackState {
    pub started: AtomicBool,
    pub connected: AtomicBool,
    pub authorized: AtomicBool,
    pub last_error: Mutex<String>,
    pub bot_user_id: Mutex<String>,
    pub team_id: Mutex<String>,
    /// HTTP client for `chat.postMessage`; set after `auth.test` succeeds.
    pub client: Mutex<Option<Arc<SlackHyperClient>>>,
    /// Bot token for API sessions; set after `auth.test` succeeds.
    pub bot_token: Mutex<Option<SlackApiToken>>,
    pub event_tx: tokio::sync::broadcast::Sender<OutboundEvent>,
}

impl SlackState {
    pub fn new(event_tx: tokio::sync::broadcast::Sender<OutboundEvent>) -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            authorized: AtomicBool::new(false),
            last_error: Mutex::new(String::new()),
            bot_user_id: Mutex::new(String::new()),
            team_id: Mutex::new(String::new()),
            client: Mutex::new(None),
            bot_token: Mutex::new(None),
            event_tx,
        })
    }

    pub fn set_error(&self, msg: &str) {
        *self.last_error.lock().expect("last_error poisoned") = msg.to_string();
    }

    pub fn error_text(&self) -> String {
        self.last_error.lock().expect("last_error poisoned").clone()
    }

    pub fn emit_connection_status(&self) {
        let mut data = serde_json::json!({
            "connected":  self.connected.load(Ordering::Acquire),
            "authorized": self.authorized.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        let err = self.error_text();
        if !err.is_empty() {
            data["last_error"] = serde_json::Value::String(err);
        }
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        if !bid.is_empty() {
            data["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            data["team_id"] = serde_json::Value::String(tid);
        }
        let _ = self.event_tx.send(OutboundEvent::new("slack.connection.status", data));
    }

    pub fn status_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "running":    self.started.load(Ordering::Acquire),
            "connected":  self.connected.load(Ordering::Acquire),
            "authorized": self.authorized.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        let err = self.error_text();
        if !err.is_empty() {
            v["last_error"] = serde_json::Value::String(err);
        }
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        if !bid.is_empty() {
            v["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            v["team_id"] = serde_json::Value::String(tid);
        }
        v
    }

    pub fn link_state_json(&self) -> serde_json::Value {
        let bid = self.bot_user_id.lock().expect("bot_user_id poisoned").clone();
        let tid = self.team_id.lock().expect("team_id poisoned").clone();
        let mut v = serde_json::json!({
            "authorized": self.authorized.load(Ordering::Acquire),
            "connected":  self.connected.load(Ordering::Acquire),
            "backend":    BACKEND,
        });
        if !bid.is_empty() {
            v["bot_user_id"] = serde_json::Value::String(bid);
        }
        if !tid.is_empty() {
            v["team_id"] = serde_json::Value::String(tid);
        }
        v
    }
}
