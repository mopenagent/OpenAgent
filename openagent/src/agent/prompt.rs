//! Prompt rendering via MiniJinja.
//!
//! Templates are embedded at compile time with `include_str!` — no file I/O
//! at runtime, no missing-file failures on a Pi.  The `Environment` is built
//! once and cached in a `OnceLock`.
//!
//! # Template files (relative to this source file)
//!
//! | File                               | Purpose                                                    |
//! |------------------------------------|------------------------------------------------------------|
//! | `prompts/step_system.j2`           | Base system prompt (trims whitespace, nothing else)        |
//! | `prompts/skill_context.j2`         | Appends matched skill summaries block to the system prompt |
//! | `prompts/diary_entry.j2`           | Deterministic markdown template for diary entries          |

use anyhow::{anyhow, Result};
use minijinja::{context, Environment};
use std::sync::OnceLock;

const STEP_SYSTEM_SRC: &str = include_str!("prompts/step_system.j2");
const SKILL_CONTEXT_SRC: &str = include_str!("prompts/skill_context.j2");
const DIARY_ENTRY_SRC: &str = include_str!("prompts/diary_entry.j2");

static ENV: OnceLock<Environment<'static>> = OnceLock::new();

fn env() -> &'static Environment<'static> {
    ENV.get_or_init(|| {
        let mut e = Environment::new();
        e.add_template("step_system", STEP_SYSTEM_SRC)
            .expect("step_system.j2 must be a valid MiniJinja template");
        e.add_template("skill_context", SKILL_CONTEXT_SRC)
            .expect("skill_context.j2 must be a valid MiniJinja template");
        e.add_template("diary_entry", DIARY_ENTRY_SRC)
            .expect("diary_entry.j2 must be a valid MiniJinja template");
        e
    })
}

/// Render the base system prompt (trims whitespace only).
pub fn render_step_system(system_prompt: &str) -> Result<String> {
    env()
        .get_template("step_system")
        .and_then(|t| t.render(context! { system_prompt => system_prompt.trim() }))
        .map_err(|e| anyhow!("step_system render failed: {e}"))
}

/// Append matched skill summaries to the system prompt.
pub fn render_skill_context(system_prompt: &str, skill_context: &str) -> Result<String> {
    let skill_context = skill_context.trim();
    if skill_context.is_empty() {
        return Ok(system_prompt.to_string());
    }
    env()
        .get_template("skill_context")
        .and_then(|t| t.render(context! {
            system_prompt  => system_prompt,
            skill_context  => skill_context,
        }))
        .map_err(|e| anyhow!("skill_context render failed: {e}"))
}

/// Input for `render_diary_entry`.
#[derive(Debug)]
pub struct DiaryEntryContext<'a> {
    pub session_id:    &'a str,
    pub timestamp:     u64,
    pub user_input:    &'a str,
    pub response_text: &'a str,
    pub tool_calls:    &'a [String],
}

/// Render a deterministic markdown diary entry.
pub fn render_diary_entry(ctx: &DiaryEntryContext<'_>) -> Result<String> {
    env()
        .get_template("diary_entry")
        .and_then(|t| {
            t.render(context! {
                session_id    => ctx.session_id,
                timestamp     => ctx.timestamp,
                user_input    => ctx.user_input,
                response_text => ctx.response_text,
                tool_calls    => ctx.tool_calls,
            })
        })
        .map_err(|e| anyhow!("diary_entry render failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_system_returns_trimmed_prompt() {
        let out = render_step_system("  You are a helpful assistant.  ").unwrap();
        assert_eq!(out, "You are a helpful assistant.");
    }

    #[test]
    fn skill_context_appends_skills_block() {
        let base = "Base system prompt.";
        let skills = "skill: agent-browser\ndescription: Browser automation.";
        let out = render_skill_context(base, skills).unwrap();
        assert!(out.starts_with(base));
        assert!(out.contains("## Available Skills"));
        assert!(out.contains("agent-browser"));
    }

    #[test]
    fn skill_context_returns_bare_prompt_when_empty() {
        let base = "Base system prompt.";
        let out = render_skill_context(base, "").unwrap();
        assert_eq!(out, base);
    }

    #[test]
    fn diary_entry_contains_required_sections() {
        let ctx = DiaryEntryContext {
            session_id:    "sess-abc",
            timestamp:     1_700_000_000,
            user_input:    "What is the capital of France?",
            response_text: "Paris.",
            tool_calls:    &[],
        };
        let out = render_diary_entry(&ctx).unwrap();
        assert!(out.contains("# Session: sess-abc"));
        assert!(out.contains("1700000000"));
        assert!(out.contains("Paris."));
        assert!(out.contains("_none_"));
    }
}
