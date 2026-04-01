//! Tool definitions and MCP-lite handler registration for the research service.

use crate::db::ResearchStore;
use crate::handlers::{
    handle_research_complete, handle_research_list, handle_research_start, handle_research_status,
    handle_research_switch, handle_task_add, handle_task_done, handle_task_fail,
};
use crate::metrics::ResearchTelemetry;
use sdk_rust::{McpLiteServer, ToolDefinition};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub fn make_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "research.start".to_string(),
            description: concat!(
                "Start a new research session with a goal. Creates a research entry, ",
                "sets it as the current research for the user, and adds initial skeleton tasks. ",
                "Returns the research object."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier (e.g. telegram:12345 or web:session_id)"
                    },
                    "goal": {
                        "type": "string",
                        "description": "The research goal or question to investigate"
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional short title (defaults to first 60 chars of goal)"
                    }
                },
                "required": ["user_key", "goal"]
            }),
        },
        ToolDefinition {
            name: "research.list".to_string(),
            description: concat!(
                "List all research sessions for a user, newest first. ",
                "Returns an array of research objects."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier"
                    }
                },
                "required": ["user_key"]
            }),
        },
        ToolDefinition {
            name: "research.switch".to_string(),
            description: concat!(
                "Switch the active research session for a user to the specified research_id. ",
                "Returns the updated research object."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier"
                    },
                    "research_id": {
                        "type": "string",
                        "description": "Research ID to switch to"
                    }
                },
                "required": ["user_key", "research_id"]
            }),
        },
        ToolDefinition {
            name: "research.status".to_string(),
            description: concat!(
                "Get the current research for a user along with all tasks and runnable tasks. ",
                "Returns {research, tasks, runnable_tasks}."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier"
                    }
                },
                "required": ["user_key"]
            }),
        },
        ToolDefinition {
            name: "research.complete".to_string(),
            description: concat!(
                "Mark the current research session as complete. ",
                "Updates status to 'complete' and writes a final snapshot."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier"
                    }
                },
                "required": ["user_key"]
            }),
        },
        ToolDefinition {
            name: "research.task_add".to_string(),
            description: concat!(
                "Add a new task to the current research session. ",
                "Supports optional dependency declarations and parent task hierarchy. ",
                "Returns the created task object."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "user_key": {
                        "type": "string",
                        "description": "User identifier"
                    },
                    "description": {
                        "type": "string",
                        "description": "Task description"
                    },
                    "depends_on": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of task IDs this task depends on"
                    },
                    "parent_id": {
                        "type": "string",
                        "description": "Optional parent task ID for hierarchical tasks"
                    },
                    "assigned_agent": {
                        "type": "string",
                        "description": "Optional agent name to assign this task to"
                    }
                },
                "required": ["user_key", "description"]
            }),
        },
        ToolDefinition {
            name: "research.task_done".to_string(),
            description: concat!(
                "Mark a task as done with a result string. ",
                "Updates status to 'done' and triggers a snapshot write."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to mark as done"
                    },
                    "result": {
                        "type": "string",
                        "description": "Result or output from completing this task"
                    }
                },
                "required": ["task_id", "result"]
            }),
        },
        ToolDefinition {
            name: "research.task_fail".to_string(),
            description: concat!(
                "Mark a task as failed with a reason. ",
                "Updates status to 'failed' and triggers a snapshot write."
            )
            .to_string(),
            params: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to mark as failed"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Reason the task failed"
                    }
                },
                "required": ["task_id", "reason"]
            }),
        },
    ]
}

pub fn register_handlers(
    server: &mut McpLiteServer,
    store: Arc<ResearchStore>,
    research_dir: Arc<PathBuf>,
    tel: Arc<ResearchTelemetry>,
) {
    let (s1, d1, t1) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.start", move |p| {
        let (s, d, t) = (Arc::clone(&s1), Arc::clone(&d1), Arc::clone(&t1));
        async move { handle_research_start(p, s, d, t) }
    });

    let (s2, t2) = (Arc::clone(&store), Arc::clone(&tel));
    server.register_tool("research.list", move |p| {
        let (s, t) = (Arc::clone(&s2), Arc::clone(&t2));
        async move { handle_research_list(p, s, t) }
    });

    let (s3, d3, t3) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.switch", move |p| {
        let (s, d, t) = (Arc::clone(&s3), Arc::clone(&d3), Arc::clone(&t3));
        async move { handle_research_switch(p, s, d, t) }
    });

    let (s4, t4) = (Arc::clone(&store), Arc::clone(&tel));
    server.register_tool("research.status", move |p| {
        let (s, t) = (Arc::clone(&s4), Arc::clone(&t4));
        async move { handle_research_status(p, s, t) }
    });

    let (s5, d5, t5) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.complete", move |p| {
        let (s, d, t) = (Arc::clone(&s5), Arc::clone(&d5), Arc::clone(&t5));
        async move { handle_research_complete(p, s, d, t) }
    });

    let (s6, d6, t6) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.task_add", move |p| {
        let (s, d, t) = (Arc::clone(&s6), Arc::clone(&d6), Arc::clone(&t6));
        async move { handle_task_add(p, s, d, t) }
    });

    let (s7, d7, t7) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.task_done", move |p| {
        let (s, d, t) = (Arc::clone(&s7), Arc::clone(&d7), Arc::clone(&t7));
        async move { handle_task_done(p, s, d, t) }
    });

    let (s8, d8, t8) = (Arc::clone(&store), Arc::clone(&research_dir), Arc::clone(&tel));
    server.register_tool("research.task_fail", move |p| {
        let (s, d, t) = (Arc::clone(&s8), Arc::clone(&d8), Arc::clone(&t8));
        async move { handle_task_fail(p, s, d, t) }
    });
}
