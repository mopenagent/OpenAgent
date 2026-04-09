//! WhatsApp Cloud API channel — Meta Business API.
//!
//! ## Mode: Cloud API (this file)
//! Uses the official Meta Cloud API. Outbound messages go via REST to
//! `graph.facebook.com`. Inbound messages arrive via Meta webhooks POSTed
//! to `POST /webhook/whatsapp` in the Axum server.
//!
//! **Current deployment:** The Go-based `services/whatsapp/` (whatsmeow) is
//! the active inbound handler. This channel is wired for outbound sends and
//! webhook parsing. Switch `dispatch.rs` platform routing once Meta Business
//! approval is obtained.
//!
//! Config block in `config/channels.toml`:
//! ```toml
//! [whatsapp]
//! enabled          = true
//! access_token     = "${WHATSAPP_ACCESS_TOKEN}"    # Meta permanent token
//! phone_number_id  = "${WHATSAPP_PHONE_NUMBER_ID}" # from Meta dashboard
//! verify_token     = "${WHATSAPP_VERIFY_TOKEN}"    # webhook challenge secret
//! allowed_numbers  = []   # E.164 numbers, empty = all (use with caution)
//! ```
//!
//! ## Migration path from Go service
//! - Go service (whatsmeow): QR-code auth, unofficial WhatsApp Web protocol,
//!   full media support, no Meta approval needed.
//! - Cloud API (this): official Meta API, requires Business verification,
//!   webhook-based inbound, production-grade reliability.
//!
//! When ready to migrate:
//! 1. Enable this config block.
//! 2. Register the webhook URL with Meta (`POST /webhook/whatsapp`).
//! 3. In `dispatch.rs`, the `platform == "whatsapp"` branch already uses
//!    `channel_handle.send()` — no dispatch changes needed once this channel
//!    is registered.
//! 4. Disable the Go `services/whatsapp/` service in `config/openagent.toml`.

use serde::Deserialize;

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use regex::Regex;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Meta permanent access token.
    #[serde(default)]
    pub access_token: String,
    /// Phone number ID from the Meta Business dashboard.
    #[serde(default)]
    pub phone_number_id: String,
    /// Secret token used to verify webhook challenge from Meta.
    #[serde(default)]
    pub verify_token: String,
    /// Allowed E.164 phone numbers. Empty or `["*"]` = allow all.
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
}

/// Build a WhatsApp Cloud API channel.
///
/// Outbound sends work immediately. Inbound requires a running webhook
/// handler at `POST /webhook/whatsapp` that calls
/// [`parse_webhook`] and injects events onto the dispatch bus.
pub fn build(cfg: &WhatsAppConfig) -> WhatsAppChannel {
    WhatsAppChannel::new(
        cfg.access_token.clone(),
        cfg.phone_number_id.clone(),
        cfg.verify_token.clone(),
        cfg.allowed_numbers.clone(),
    )
}

/// Parse a raw Meta webhook POST body into `message.received` event payloads.
///
/// Called from the Axum webhook route. Each returned [`serde_json::Value`]
/// is ready to be sent on the `ServiceManager` broadcast bus so the dispatch
/// loop picks it up exactly like any other channel event.
pub fn parse_webhook(
    cfg: &WhatsAppConfig,
    payload: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let inner = WhatsAppChannel::new(
        cfg.access_token.clone(),
        cfg.phone_number_id.clone(),
        cfg.verify_token.clone(),
        cfg.allowed_numbers.clone(),
    );
    inner
        .parse_webhook_payload(payload)
        .into_iter()
        .map(|msg| {
            serde_json::json!({
                "type":  "event",
                "event": "message.received",
                "data": {
                    "id":      msg.id,
                    "sender":  msg.sender,
                    "content": msg.content,
                    "channel": format!("whatsapp://{}", msg.channel),
                    "timestamp": msg.timestamp,
                }
            })
        })
        .collect()
}

/// `WhatsApp` channel — uses `WhatsApp` Business Cloud API
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/whatsapp` webhook endpoint.
/// The `listen` method here is a no-op placeholder; actual message handling
/// happens in the gateway when Meta sends webhook events.
fn ensure_https(url: &str) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!(
            "Refusing to transmit sensitive data over non-HTTPS URL: URL scheme must be https"
        );
    }
    Ok(())
}

///
/// # Runtime Negotiation
///
/// This Cloud API channel is automatically selected when `phone_number_id` is set in the config.
/// Use `WhatsAppWebChannel` (with `session_path`) for native Web mode.
#[derive(Debug)]
pub struct WhatsAppChannel {
    access_token: String,
    endpoint_id: String,
    verify_token: String,
    allowed_numbers: Vec<String>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Compiled mention patterns for DM mention gating.
    dm_mention_patterns: Vec<Regex>,
    /// Compiled mention patterns for group-chat mention gating.
    group_mention_patterns: Vec<Regex>,
}

impl WhatsAppChannel {
    pub fn new(
        access_token: String,
        endpoint_id: String,
        verify_token: String,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            access_token,
            endpoint_id,
            verify_token,
            allowed_numbers,
            proxy_url: None,
            dm_mention_patterns: Vec::new(),
            group_mention_patterns: Vec::new(),
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Set mention patterns for DM mention gating.
    /// Each pattern string is compiled as a case-insensitive regex.
    /// Invalid patterns are logged and skipped.
    pub fn with_dm_mention_patterns(mut self, patterns: Vec<String>) -> Self {
        self.dm_mention_patterns = Self::compile_mention_patterns(&patterns);
        self
    }

    /// Set mention patterns for group-chat mention gating.
    /// Each pattern string is compiled as a case-insensitive regex.
    /// Invalid patterns are logged and skipped.
    pub fn with_group_mention_patterns(mut self, patterns: Vec<String>) -> Self {
        self.group_mention_patterns = Self::compile_mention_patterns(&patterns);
        self
    }

    /// Compile raw pattern strings into case-insensitive regexes.
    /// Invalid or excessively large patterns are logged and skipped.
    pub(crate) fn compile_mention_patterns(patterns: &[String]) -> Vec<Regex> {
        patterns
            .iter()
            .filter_map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    return None;
                }
                match regex::RegexBuilder::new(trimmed)
                    .case_insensitive(true)
                    .size_limit(1 << 16) // 64 KiB — guard against ReDoS
                    .build()
                {
                    Ok(re) => Some(re),
                    Err(e) => {
                        tracing::warn!(
                            "WhatsApp: ignoring invalid mention_pattern {trimmed:?}: {e}"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Check whether `text` matches any pattern in the given slice.
    pub(crate) fn text_matches_patterns(patterns: &[Regex], text: &str) -> bool {
        patterns.iter().any(|re| re.is_match(text))
    }

    /// Strip all pattern matches from `text`, collapse whitespace,
    /// and return `None` if the result is empty.
    pub(crate) fn strip_patterns(patterns: &[Regex], text: &str) -> Option<String> {
        let mut result = text.to_string();
        for re in patterns {
            result = re.replace_all(&result, " ").into_owned();
        }
        let normalized = result.split_whitespace().collect::<Vec<_>>().join(" ");
        (!normalized.is_empty()).then_some(normalized)
    }

    /// Apply mention-pattern gating for a message.
    ///
    /// Selects the appropriate pattern set based on `is_group` and applies
    /// mention gating: when patterns are non-empty, messages that do not
    /// match any pattern are dropped (`None`); messages that match have
    /// the matched fragments stripped.
    /// When the applicable pattern set is empty the original content is
    /// returned unchanged.
    pub(crate) fn apply_mention_gating(
        dm_patterns: &[Regex],
        group_patterns: &[Regex],
        content: &str,
        is_group: bool,
    ) -> Option<String> {
        let patterns = if is_group {
            group_patterns
        } else {
            dm_patterns
        };
        if patterns.is_empty() {
            return Some(content.to_string());
        }
        if !Self::text_matches_patterns(patterns, content) {
            return None;
        }
        Self::strip_patterns(patterns, content)
    }

    /// Detect group messages in the WhatsApp Cloud API webhook payload.
    ///
    /// A message is considered a group message when it carries a `context`
    /// object containing a non-empty `group_id` field.
    fn is_group_message(msg: &serde_json::Value) -> bool {
        msg.get("context")
            .and_then(|ctx| ctx.get("group_id"))
            .and_then(|g| g.as_str())
            .is_some_and(|s| !s.is_empty())
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client("channel.whatsapp", self.proxy_url.as_deref())
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890)
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
    }

    /// Get the verify token for webhook verification
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Parse an incoming webhook payload from Meta and extract messages
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // WhatsApp Cloud API webhook structure:
        // { "object": "whatsapp_business_account", "entry": [...] }
        let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) else {
            return messages;
        };

        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) else {
                continue;
            };

            for change in changes {
                let Some(value) = change.get("value") else {
                    continue;
                };

                // Extract messages array
                let Some(msgs) = value.get("messages").and_then(|m| m.as_array()) else {
                    continue;
                };

                for msg in msgs {
                    // Get sender phone number
                    let Some(from) = msg.get("from").and_then(|f| f.as_str()) else {
                        continue;
                    };

                    // Check allowlist
                    let normalized_from = if from.starts_with('+') {
                        from.to_string()
                    } else {
                        format!("+{from}")
                    };

                    if !self.is_number_allowed(&normalized_from) {
                        tracing::warn!(
                            "WhatsApp: ignoring message from unauthorized number: {normalized_from}. \
                            Add to channels.whatsapp.allowed_numbers in config.toml, \
                            or run `zeroclaw onboard --channels-only` to configure interactively."
                        );
                        continue;
                    }

                    // Extract text content (support text messages only for now)
                    let content = if let Some(text_obj) = msg.get("text") {
                        text_obj
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        // Could be image, audio, etc. — skip for now
                        tracing::debug!("WhatsApp: skipping non-text message from {from}");
                        continue;
                    };

                    if content.is_empty() {
                        continue;
                    }

                    // Mention-pattern gating: apply dm_mention_patterns for
                    // DMs and group_mention_patterns for groups. When the
                    // applicable pattern set is non-empty, messages without a
                    // match are dropped and matched fragments are stripped.
                    let is_group = Self::is_group_message(msg);
                    let content = match Self::apply_mention_gating(
                        &self.dm_mention_patterns,
                        &self.group_mention_patterns,
                        &content,
                        is_group,
                    ) {
                        Some(c) => c,
                        None => {
                            tracing::debug!(
                                "WhatsApp: message from {from} did not match mention patterns, dropping"
                            );
                            continue;
                        }
                    };

                    // Get timestamp
                    let timestamp = msg
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|t| t.parse::<u64>().ok())
                        .unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                        });

                    messages.push(ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        reply_target: normalized_from.clone(),
                        sender: normalized_from,
                        content,
                        channel: "whatsapp".to_string(),
                        timestamp,
                        thread_ts: None,
                        interruption_scope_id: None,
                        attachments: vec![],
                    });
                }
            }
        }

        messages
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // WhatsApp Cloud API: POST to /v18.0/{phone_number_id}/messages
        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.endpoint_id
        );

        // Normalize recipient (remove leading + if present for API)
        let to = message
            .recipient
            .strip_prefix('+')
            .unwrap_or(&message.recipient);

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": message.content
            }
        });

        ensure_https(&url)?;

        let resp = self
            .http_client()
            .post(&url)
            .bearer_auth(&self.access_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            tracing::error!("WhatsApp send failed: {status} — {error_body}");
            anyhow::bail!("WhatsApp API error: {status}");
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WhatsApp uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /whatsapp endpoint.
        // This method keeps the channel "alive" but doesn't actively poll.
        tracing::info!(
            "WhatsApp channel active (webhook mode). \
            Configure Meta webhook to POST to your gateway's /whatsapp endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Check if we can reach the WhatsApp API
        let url = format!("https://graph.facebook.com/v18.0/{}", self.endpoint_id);

        if ensure_https(&url).is_err() {
            return false;
        }

        self.http_client()
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}
