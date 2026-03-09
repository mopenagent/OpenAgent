//! Tool definitions and MCP-lite handler registration for the memory service.

use crate::handlers::{handle_index_trace, handle_search_memory};
use crate::metrics::MetricsWriter;
use fastembed::TextEmbedding;
use lancedb::connection::Connection;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "memory.index_trace".to_string(),
            description: concat!(
                "Embed and store text (content + optional metadata) into ",
                "LTS (long-term summaries) or STS (short-term conversation chain). ",
                "Returns the generated document id."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Text to embed and store"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Optional metadata (session_id, source, etc.)"
                    },
                    "store": {
                        "type": "string",
                        "enum": ["lts", "sts"],
                        "description": "lts = long-term summaries; sts = short-term full chain"
                    }
                },
                "required": ["content", "store"]
            }),
        },
        ToolDefinition {
            name: "memory.search_memory".to_string(),
            description: concat!(
                "Semantic vector search over LTS, STS, or both. ",
                "Returns up to 5 results ranked by similarity score (0–1, higher = more relevant). ",
                "Each result includes id, content, metadata, created_at, store, score, distance."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language query"
                    },
                    "store": {
                        "type": "string",
                        "enum": ["lts", "sts", "all"],
                        "description": "Store to search (default: all — merges and re-ranks by score)"
                    }
                },
                "required": ["query"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    metrics: Arc<MetricsWriter>,
) {
    let (db1, m1, mx1) = (Arc::clone(&db), Arc::clone(&model), Arc::clone(&metrics));
    server.register_tool("memory.index_trace", move |p| {
        handle_index_trace(p, Arc::clone(&db1), Arc::clone(&m1), Arc::clone(&mx1))
    });

    let (db2, m2, mx2) = (Arc::clone(&db), Arc::clone(&model), Arc::clone(&metrics));
    server.register_tool("memory.search_memory", move |p| {
        handle_search_memory(p, Arc::clone(&db2), Arc::clone(&m2), Arc::clone(&mx2))
    });
}
