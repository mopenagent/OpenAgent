//! Tool definitions and MCP-lite handler registration for the memory service.

use crate::handlers::{handle_delete, handle_diary_write, handle_index, handle_prune, handle_search};
use crate::metrics::MemoryTelemetry;
use fastembed::TextEmbedding;
use lancedb::connection::Connection;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "memory.index".to_string(),
            description: concat!(
                "Embed and store text (content + optional metadata) into long-term memory. ",
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
                        "description": "Optional metadata (session_id, source, user_id, type, tags, etc.)"
                    },
                    "store": {
                        "type": "string",
                        "enum": ["memory"],
                        "description": "memory = long-term memory store"
                    }
                },
                "required": ["content", "store"]
            }),
        },
        ToolDefinition {
            name: "memory.search".to_string(),
            description: concat!(
                "Hybrid (dense + BM25 + RRF) search over memory, diary, knowledge, or all stores. ",
                "Returns up to 5 results ranked by RRF score. ",
                "Each result includes id, content, metadata, created_at, store, rrf_score, dense_score."
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
                        "enum": ["memory", "diary", "knowledge", "all"],
                        "description": "Store to search (default: all — merges and re-ranks by RRF score)"
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "memory.delete".to_string(),
            description: concat!(
                "Delete a specific document by id from a store, ",
                "or purge all documents from a store by omitting id."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "store": {
                        "type": "string",
                        "enum": ["memory", "diary", "knowledge"],
                        "description": "Store to delete from"
                    },
                    "id": {
                        "type": "string",
                        "description": "Document id to delete. Omit to purge the entire store."
                    }
                },
                "required": ["store"]
            }),
        },
        ToolDefinition {
            name: "memory.prune".to_string(),
            description: concat!(
                "Remove stale entries from the diary store. ",
                "Deletes every row older than max_age_secs (default 86400 = 24 h). ",
                "Returns the number of documents pruned."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "max_age_secs": {
                        "type": "integer",
                        "description": "Age threshold in seconds (default: 86400 = 24 h). Documents older than this are deleted."
                    }
                },
                "required": []
            }),
        },
        ToolDefinition {
            name: "memory.diary_write".to_string(),
            description: concat!(
                "Write a stub diary row (zero vector placeholder) for a completed ReAct turn. ",
                "Called fire-and-forget by Cortex after each final answer. ",
                "Compaction will back-fill real embeddings in a future pass."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Session identifier"
                    },
                    "content": {
                        "type": "string",
                        "description": "Truncated response text (up to 500 chars)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the diary markdown file"
                    }
                },
                "required": ["session_id", "content"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    db: Arc<Connection>,
    model: Arc<Mutex<TextEmbedding>>,
    tel: Arc<MemoryTelemetry>,
) {
    let (db1, m1, t1) = (Arc::clone(&db), Arc::clone(&model), Arc::clone(&tel));
    server.register_tool("memory.index", move |p| {
        handle_index(p, Arc::clone(&db1), Arc::clone(&m1), Arc::clone(&t1))
    });

    let (db2, m2, t2) = (Arc::clone(&db), Arc::clone(&model), Arc::clone(&tel));
    server.register_tool("memory.search", move |p| {
        handle_search(p, Arc::clone(&db2), Arc::clone(&m2), Arc::clone(&t2))
    });

    let (db3, t3) = (Arc::clone(&db), Arc::clone(&tel));
    server.register_tool("memory.delete", move |p| {
        handle_delete(p, Arc::clone(&db3), Arc::clone(&t3))
    });

    let (db4, t4) = (Arc::clone(&db), Arc::clone(&tel));
    server.register_tool("memory.prune", move |p| {
        handle_prune(p, Arc::clone(&db4), Arc::clone(&t4))
    });

    let (db5, t5) = (Arc::clone(&db), Arc::clone(&tel));
    server.register_tool("memory.diary_write", move |p| {
        handle_diary_write(p, Arc::clone(&db5), Arc::clone(&t5))
    });
}
