use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Tool,
    SkillGuidance,
}

impl ActionKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::SkillGuidance => "skill_guidance",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActionCatalog {
    entries: Vec<ActionEntry>,
}

impl ActionCatalog {
    pub fn discover_from_root(root: &Path) -> Result<Self> {
        let mut entries = Self::discover_from_services_dir(&root.join("services"))?.entries;
        let mut skill_entries = Self::discover_from_skills_dir(&root.join("skills"))?.entries;
        entries.append(&mut skill_entries);
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self { entries })
    }

    pub fn discover_from_services_dir(services_dir: &Path) -> Result<Self> {
        let mut entries = Vec::new();
        let dir_iter = fs::read_dir(services_dir)
            .with_context(|| format!("failed to read services dir {}", services_dir.display()))?;

        for item in dir_iter {
            let item = item?;
            let manifest_path = item.path().join("service.json");
            if !manifest_path.is_file() {
                continue;
            }

            let raw = fs::read_to_string(&manifest_path)
                .with_context(|| format!("failed to read {}", manifest_path.display()))?;
            let manifest: ServiceManifest = serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

            for tool in manifest.tools {
                entries.push(ActionEntry::from_tool(
                    manifest.name.clone(),
                    manifest.runtime.clone(),
                    manifest_path.clone(),
                    tool,
                ));
            }
        }

        Ok(Self { entries })
    }

    pub fn discover_from_skills_dir(skills_dir: &Path) -> Result<Self> {
        let mut entries = Vec::new();
        if !skills_dir.is_dir() {
            return Ok(Self { entries });
        }

        let dir_iter = fs::read_dir(skills_dir)
            .with_context(|| format!("failed to read skills dir {}", skills_dir.display()))?;

        for item in dir_iter {
            let item = item?;
            let skill_path = item.path().join("SKILL.md");
            if !skill_path.is_file() {
                continue;
            }
            let raw = fs::read_to_string(&skill_path)
                .with_context(|| format!("failed to read {}", skill_path.display()))?;
            let entry = ActionEntry::from_skill(skill_path, &raw)?;
            // Only load skills that have explicitly opted in with `enabled: true`.
            // Skills without the field (or with `enabled: false`) are not loaded into
            // the catalog — they don't appear in search or in the action context.
            if entry.enabled {
                entries.push(entry);
            }
        }

        Ok(Self { entries })
    }

    #[must_use]
    pub fn entries(&self) -> &[ActionEntry] {
        &self.entries
    }
}

#[derive(Clone, Debug)]
pub struct ActionEntry {
    pub kind: ActionKind,
    pub owner: String,
    pub runtime: String,
    pub manifest_path: PathBuf,
    pub name: String,
    /// Short description shown in semantic search. For skills: description + hint line.
    pub summary: String,
    pub params: Value,
    pub required: Vec<String>,
    pub param_names: Vec<String>,
    pub allowed_tools: Vec<String>,
    /// When true, Cortex enforces allowed_tools — rejects calls outside the list.
    pub enforce: bool,
    /// When false (default), this skill is not loaded into the catalog.
    pub enabled: bool,
    pub steps: Vec<String>,
    pub constraints: Vec<String>,
    pub completion_criteria: Vec<String>,
    pub guidance: String,
    pub search_blob: String,
}

impl ActionEntry {
    fn from_tool(
        owner: String,
        runtime: String,
        manifest_path: PathBuf,
        tool: ManifestTool,
    ) -> Self {
        let required = tool
            .params
            .get("required")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let param_names = tool
            .params
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| props.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();

        let guidance = format!(
            "Use service tool {} from {} when the request matches this capability.",
            tool.name, owner
        );
        let search_blob = build_search_blob(
            ActionKind::Tool.as_str(),
            &owner,
            &runtime,
            &tool.name,
            &tool.description,
            &param_names,
            &required,
            &[],
            &[],
            &[],
            &guidance,
        );

        Self {
            kind: ActionKind::Tool,
            owner,
            runtime,
            manifest_path,
            name: tool.name,
            summary: tool.description,
            params: tool.params,
            required,
            param_names,
            allowed_tools: Vec::new(),
            enforce: false,
            enabled: true,  // tools are always enabled; filtering is per skill only
            steps: Vec::new(),
            constraints: Vec::new(),
            completion_criteria: Vec::new(),
            guidance,
            search_blob,
        }
    }

    fn from_skill(manifest_path: PathBuf, raw: &str) -> Result<Self> {
        let parsed = parse_skill_file(raw)?;
        let owner = manifest_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            .unwrap_or("skills")
            .to_string();

        let description = parsed
            .frontmatter
            .description
            .clone()
            .unwrap_or_else(|| "Local skill guidance".to_string());

        // summary = description + hint line (if present).
        // The hint is rendered in the action context so the LLM knows exactly how
        // to invoke skill.read for this skill.
        let summary = match parsed.frontmatter.hint.as_deref() {
            Some(hint) if !hint.is_empty() => format!("{}\nhint: {}", description, hint),
            _ => description,
        };

        let name = parsed
            .frontmatter
            .name
            .clone()
            .unwrap_or_else(|| owner.clone());
        let enforce = parsed.frontmatter.enforce;
        let enabled = parsed.frontmatter.enabled;
        let search_blob = build_search_blob(
            ActionKind::SkillGuidance.as_str(),
            &owner,
            "markdown",
            &name,
            &summary,
            &[],
            &[],
            &parsed.allowed_tools,
            &parsed.steps,
            &parsed.constraints,
            &parsed.guidance,
        );

        Ok(Self {
            kind: ActionKind::SkillGuidance,
            owner,
            runtime: "markdown".to_string(),
            manifest_path,
            name,
            summary,
            params: Value::Null,
            required: Vec::new(),
            param_names: Vec::new(),
            allowed_tools: parsed.allowed_tools,
            enforce,
            enabled,
            steps: parsed.steps,
            constraints: parsed.constraints,
            completion_criteria: parsed.completion_criteria,
            guidance: parsed.guidance,
            search_blob,
        })
    }
}

fn build_search_blob(
    kind: &str,
    owner: &str,
    runtime: &str,
    name: &str,
    summary: &str,
    param_names: &[String],
    required: &[String],
    allowed_tools: &[String],
    steps: &[String],
    constraints: &[String],
    guidance: &str,
) -> String {
    let mut parts = vec![
        kind.to_lowercase(),
        owner.to_lowercase(),
        runtime.to_lowercase(),
        name.to_lowercase(),
        summary.to_lowercase(),
        guidance.to_lowercase(),
    ];
    parts.extend(param_names.iter().map(|v| v.to_lowercase()));
    parts.extend(required.iter().map(|v| v.to_lowercase()));
    parts.extend(allowed_tools.iter().map(|v| v.to_lowercase()));
    parts.extend(steps.iter().map(|v| v.to_lowercase()));
    parts.extend(constraints.iter().map(|v| v.to_lowercase()));
    parts.join(" ")
}

#[derive(Debug, Deserialize)]
struct ServiceManifest {
    name: String,
    #[serde(default)]
    runtime: String,
    #[serde(default)]
    tools: Vec<ManifestTool>,
}

#[derive(Debug, Deserialize)]
struct ManifestTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    params: Value,
}

/// Frontmatter parsed from SKILL.md.
///
/// `enabled` is the gate — only skills with `enabled: true` are loaded into the
/// ActionCatalog and appear in the LLM's action context.  Default is `false`
/// (opt-in) so new skills are invisible until explicitly activated.
///
/// `hint` is appended to `description` in the rendered context block so the LLM
/// knows exactly which `skill.read(name=...)` call to make for this skill.
///
/// `allowed-tools` (preferred) or `tools` both accepted for backward compat.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// One-line call-to-action appended to description in context.
    /// Example: `hint: Call skill.read(name="agent-browser") for commands and patterns.`
    #[serde(default)]
    hint: Option<String>,
    /// When true, this skill is loaded into the ActionCatalog. Default false (opt-in).
    #[serde(default)]
    enabled: bool,
    /// Preferred key for listing the tools this skill uses.
    #[serde(rename = "allowed-tools", default)]
    allowed_tools: Option<Value>,
    /// Legacy alias for `allowed-tools`. Ignored when `allowed-tools` is present.
    #[serde(default)]
    tools: Option<Value>,
    /// When true, Cortex rejects tool calls outside `allowed-tools`. Default false.
    #[serde(default)]
    enforce: bool,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug)]
struct ParsedSkill {
    frontmatter: SkillFrontmatter,
    allowed_tools: Vec<String>,
    steps: Vec<String>,
    constraints: Vec<String>,
    completion_criteria: Vec<String>,
    guidance: String,
}

fn parse_skill_file(raw: &str) -> Result<ParsedSkill> {
    let (frontmatter, body) = if let Some(stripped) = raw.strip_prefix("---\n") {
        if let Some((yaml, body)) = stripped.split_once("\n---\n") {
            (serde_yaml::from_str::<SkillFrontmatter>(yaml)?, body)
        } else {
            (SkillFrontmatter::default(), raw)
        }
    } else {
        (SkillFrontmatter::default(), raw)
    };

    Ok(ParsedSkill {
        // `allowed-tools` takes precedence; `tools` is the legacy alias.
        allowed_tools: parse_allowed_tools(
            frontmatter.allowed_tools.as_ref().or(frontmatter.tools.as_ref())
        ),
        steps: extract_numbered_steps(body),
        constraints: extract_section_bullets(body, &["constraints", "guardrails"]),
        completion_criteria: extract_section_bullets(
            body,
            &["completion criteria", "done when", "success criteria"],
        ),
        guidance: body.trim().to_string(),
        frontmatter,
    })
}

fn parse_allowed_tools(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_numbered_steps(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .filter_map(|line| {
            let mut chars = line.chars();
            let mut digits = String::new();
            while let Some(ch) = chars.next() {
                if ch.is_ascii_digit() {
                    digits.push(ch);
                    continue;
                }
                if !digits.is_empty() && ch == '.' {
                    let rest = chars.as_str().trim();
                    return (!rest.is_empty()).then(|| rest.to_string());
                }
                break;
            }
            None
        })
        .collect()
}

fn extract_section_bullets(body: &str, headings: &[&str]) -> Vec<String> {
    let mut active = false;
    let mut out = Vec::new();
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') {
            let heading = line.trim_start_matches('#').trim().to_lowercase();
            active = headings.iter().any(|candidate| heading.contains(candidate));
            continue;
        }
        if !active {
            continue;
        }
        if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let text = text.trim();
            if !text.is_empty() {
                out.push(text.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ActionCatalog, ActionKind};
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discovers_tools_and_skills_from_root() {
        let temp = unique_temp_dir("cortex-action-catalog");
        let services_dir = temp.join("services").join("demo");
        let skills_dir = temp.join("skills").join("demo-skill");
        fs::create_dir_all(&services_dir).unwrap();
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            services_dir.join("service.json"),
            r#"{
              "name":"demo",
              "runtime":"rust",
              "tools":[
                {
                  "name":"demo.echo",
                  "description":"Echo text",
                  "params":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}
                }
              ]
            }"#,
        )
        .unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            r#"---
name: demo-skill
description: Demo skill
enabled: true
allowed-tools:
  - demo.echo
---

# Demo

1. Inspect input
2. Use the tool

## Constraints

- Be concise
"#,
        )
        .unwrap();

        let catalog = ActionCatalog::discover_from_root(&temp).unwrap();
        assert_eq!(catalog.entries().len(), 2);
        assert!(catalog
            .entries()
            .iter()
            .any(|entry| entry.kind == ActionKind::Tool));
        let skill = catalog
            .entries()
            .iter()
            .find(|entry| entry.kind == ActionKind::SkillGuidance)
            .unwrap();
        assert_eq!(skill.allowed_tools, vec!["demo.echo".to_string()]);
        assert_eq!(skill.steps.len(), 2);
        fs::remove_dir_all(temp).unwrap();
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{now}"))
    }
}
