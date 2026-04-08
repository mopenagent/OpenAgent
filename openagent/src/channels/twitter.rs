//! Twitter/X channel — placeholder.
//!
//! Twitter/X is not yet implemented in the zeroclaw vendor library.
//! This module reserves the path for a future implementation using the
//! Twitter API v2.
//!
//! Planned config:
//! ```toml
//! [twitter]
//! enabled           = true
//! bearer_token      = "${TWITTER_BEARER_TOKEN}"
//! api_key           = "${TWITTER_API_KEY}"
//! api_secret        = "${TWITTER_API_SECRET}"
//! access_token      = "${TWITTER_ACCESS_TOKEN}"
//! access_token_secret = "${TWITTER_ACCESS_TOKEN_SECRET}"
//! allowed_users     = []   # Twitter @handles or user IDs
//! mention_only      = true
//! ```
//!
//! Suggested implementation approach:
//! - Filtered stream (`GET /2/tweets/search/stream`) for real-time mentions
//! - DMs via `GET /2/dm_conversations` polling
//! - Reply via `POST /2/tweets` with `in_reply_to_tweet_id`
//! - Use `reqwest` with OAuth 1.0a signing for user-context endpoints

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct TwitterConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bearer_token: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_secret: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub access_token_secret: String,
    /// Allowed Twitter @handles or numeric user IDs.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default = "default_true")]
    pub mention_only: bool,
}

fn default_true() -> bool { true }
