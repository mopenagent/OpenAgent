//! Reddit channel — placeholder.
//!
//! Reddit is not yet implemented in the zeroclaw vendor library.
//! This module reserves the path for a future implementation using the
//! Reddit OAuth2 API (PRAW-compatible endpoints).
//!
//! Planned config:
//! ```toml
//! [reddit]
//! enabled        = true
//! client_id      = "${REDDIT_CLIENT_ID}"
//! client_secret  = "${REDDIT_CLIENT_SECRET}"
//! username       = "${REDDIT_USERNAME}"
//! password       = "${REDDIT_PASSWORD}"
//! subreddits     = ["r/your_subreddit"]
//! mention_only   = true   # only respond to u/ mentions
//! ```
//!
//! Suggested implementation approach:
//! - REST polling via `GET /r/{sub}/new.json` for incoming messages
//! - DMs via `GET /message/inbox.json`
//! - Reply via `POST /api/comment`
//! - Use `reqwest` + `tokio::time::interval` for the poll loop

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RedditConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// Subreddits to monitor (e.g. `["r/your_subreddit"]`).
    #[serde(default)]
    pub subreddits: Vec<String>,
    #[serde(default)]
    pub mention_only: bool,
}
