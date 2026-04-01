//! Tier-1 HTTP fetch — async reqwest client.
//!
//! Sends a realistic browser-like request (Chrome on macOS headers) to avoid
//! basic bot detection. No JavaScript execution — fast and cheap.
//!
//! Environment variables:
//!   `FETCH_TIMEOUT_SECS`   — per-request timeout  (default: 12)
//!   `FETCH_MAX_BYTES`      — max response body     (default: 2 097 152 = 2 MiB)

use anyhow::{Context, Result};
use std::env;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 12;
const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Realistic Chrome 124 on macOS — avoids trivial UA-based blocks.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) \
    Chrome/124.0.0.0 Safari/537.36";

/// Fetch `url` and return the raw HTML body (up to `FETCH_MAX_BYTES`).
///
/// Returns an error on non-2xx status or network failure.
pub async fn fetch_html(url: &str) -> Result<String> {
    let timeout_secs = env::var("FETCH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let max_bytes = env::var("FETCH_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_BYTES);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1")
        .header("Upgrade-Insecure-Requests", "1")
        .send()
        .await
        .with_context(|| format!("GET {url} failed"))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("HTTP {status} for {url}"));
    }

    let bytes = resp.bytes().await.context("failed to read response body")?;
    let capped = &bytes[..bytes.len().min(max_bytes)];
    Ok(String::from_utf8_lossy(capped).into_owned())
}
