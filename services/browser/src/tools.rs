//! Tool definitions and MCP-lite handler registration for the browser service.

use crate::cache::Cache;
use crate::handlers::{handle_fetch, handle_search};
use crate::metrics::BrowserTelemetry;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "web.search".into(),
            description: concat!(
                "STEP 1 of 2. Search the web via SearXNG. ",
                "Returns a JSON array of {url, title, snippet}. ",
                "Inspect the results, pick the most relevant URL, then call web.fetch. ",
                "Results cached 5 min."
            )
            .into(),
            params: json!({
                "type": "object",
                "properties": {
                    "query":       { "type": "string",  "description": "Search query" },
                    "max_results": { "type": "integer", "description": "Max results (default 5, max 10)" }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "web.fetch".into(),
            description: concat!(
                "STEP 2 of 2. Fetch a URL and return its content as clean Markdown. ",
                "Call this after web.search with the URL you chose. ",
                "No JavaScript — if content is sparse the page requires a real browser; ",
                "use cortex.discover to find a remote browser tool. ",
                "Results cached 1 hr."
            )
            .into(),
            params: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "URL to fetch" }
                },
                "required": ["url"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    tel: Arc<BrowserTelemetry>,
    search_cache: Arc<Mutex<Cache>>,
    fetch_cache: Arc<Mutex<Cache>>,
    searxng_url: String,
) {
    let t = Arc::clone(&tel);
    let sc = Arc::clone(&search_cache);
    let url = searxng_url.clone();
    server.register_tool("web.search", move |params| {
        let t = Arc::clone(&t);
        let sc = Arc::clone(&sc);
        let url = url.clone();
        async move { handle_search(params, t, sc, url).await }
    });

    let t = Arc::clone(&tel);
    let fc = Arc::clone(&fetch_cache);
    server.register_tool("web.fetch", move |params| {
        let t = Arc::clone(&t);
        let fc = Arc::clone(&fc);
        async move { handle_fetch(params, t, fc).await }
    });
}
