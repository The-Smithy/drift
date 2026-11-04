use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};

use crate::index::INDEX_FILE;
use crate::model::Index;

const SKILL: &str = include_str!("skill.md");
const INSTRUCTIONS: &str = include_str!("instructions.md");

const START_MARKER: &str = "<!-- drift:start -->";
const END_MARKER: &str = "<!-- drift:end -->";
const HOOK_COMMAND: &str = "drift hook";
const HOOK_TOOLS: &[&str] = &["Bash", "Grep", "Read"];

// Reads smaller than this are cheap enough that a note would cost more than it saves.
const LARGE_READ_BYTES: u64 = 8 * 1024;

// Tools with no shared location to read from, and what shows the repo already uses them.
const TOOLS_NEEDING_SETUP: &[(&str, &str)] = &[
    ("kiro", ".kiro"),
    ("junie", ".junie"),
    ("continue", ".continue"),
    ("aider", ".aider.conf.yml"),
];

// These read AGENTS.md or a shared skills folder, so `drift install` already covers them.
const TOOLS_ALREADY_COVERED: &[&str] = &[
    "amp",
    "antigravity",
    "claude",
    "cline",
    "codex",
    "copilot",
    "cursor",
    "droid",
    "gemini",
    "opencode",
    "windsurf",
    "zed",
];

// `paths` is Claude Code's own field. It isn't in the Agent Skills standard the other tools follow.
fn portable_skill() -> String {
    let mut in_paths = false;
    let mut lines = Vec::new();
    for line in SKILL.lines() {
        if line.starts_with("paths:") {
            in_paths = true;
            continue;
        }
        if in_paths && line.starts_with("  - ") {
            continue;
        }
        in_paths = false;
        lines.push(line);
    }
    lines.join("\n") + "\n"
}

fn write_skill(directory: PathBuf, content: &str) -> Result<PathBuf> {
    fs::create_dir_all(&directory)?;
    let skill = directory.join("SKILL.md");
    fs::write(&skill, content)?;
    Ok(skill)
}

pub fn install_skills(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(vec![
        write_skill(root.join(".claude").join("skills").join("drift"), SKILL)?,
        write_skill(
            root.join(".agents").join("skills").join("drift"),
            &portable_skill(),
        )?,
    ])
}

fn install_aider(root: &Path) -> Result<Vec<String>> {
    let conventions = root.join("CONVENTIONS.md");
    install_instructions(&conventions)?;
    let mut lines = vec![format!("aider: {}", conventions.display())];

    let config = root.join(".aider.conf.yml");
    let mut text = fs::read_to_string(&config).unwrap_or_default();
    if text.contains("CONVENTIONS.md") {
        return Ok(lines);
    }
    if text.lines().any(|line| line.starts_with("read:")) {
        // Changing an existing list safely would need a YAML parser, so this one is left to you.
        lines.push("aider: add CONVENTIONS.md to `read:` in .aider.conf.yml".to_string());
        return Ok(lines);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("read: CONVENTIONS.md\n");
    fs::write(&config, text)?;
    lines.push(format!("aider: {}", config.display()));
    Ok(lines)
}

fn install_tool(root: &Path, tool: &str) -> Result<Vec<String>> {
    match tool {
        "kiro" => {
            let skill = write_skill(
                root.join(".kiro").join("skills").join("drift"),
                &portable_skill(),
            )?;
            Ok(vec![format!("kiro: {}", skill.display())])
        }
        "junie" => {
            let skill = write_skill(
                root.join(".junie").join("skills").join("drift"),
                &portable_skill(),
            )?;
            Ok(vec![format!("junie: {}", skill.display())])
        }
        "continue" => {
            let directory = root.join(".continue").join("rules");
            fs::create_dir_all(&directory)?;
            let rule = directory.join("drift.md");
            fs::write(
                &rule,
                format!("---\nname: Drift\nalwaysApply: true\n---\n\n{INSTRUCTIONS}"),
            )?;
            Ok(vec![format!("continue: {}", rule.display())])
        }
        "aider" => install_aider(root),
        _ => bail!("no setup for {tool}"),
    }
}

// Sets up the tools the repo already uses, plus any asked for by name.
pub fn install_tools(root: &Path, requested: &[String]) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    let mut wanted: Vec<&str> = Vec::new();
    for name in requested {
        let name = name.to_lowercase();
        if name == "all" {
            wanted.extend(TOOLS_NEEDING_SETUP.iter().map(|(tool, _)| *tool));
        } else if let Some((tool, _)) = TOOLS_NEEDING_SETUP.iter().find(|(tool, _)| *tool == name) {
            wanted.push(tool);
        } else if TOOLS_ALREADY_COVERED.contains(&name.as_str()) {
            lines.push(format!("{name}: covered by the shared files"));
        } else {
            let known: Vec<&str> = TOOLS_NEEDING_SETUP
                .iter()
                .map(|(tool, _)| *tool)
                .chain(TOOLS_ALREADY_COVERED.iter().copied())
                .collect();
            bail!(
                "Unknown tool '{name}'. Known tools: all, {}",
                known.join(", ")
            );
        }
    }
    for (tool, marker) in TOOLS_NEEDING_SETUP {
        if wanted.contains(tool) || root.join(marker).exists() {
            lines.extend(install_tool(root, tool)?);
        }
    }
    Ok(lines)
}

pub fn ignore_index(root: &Path) -> Result<bool> {
    let path = root.join(".gitignore");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|line| line.trim() == INDEX_FILE) {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(INDEX_FILE);
    text.push('\n');
    fs::write(&path, text)?;
    Ok(true)
}

// AGENTS.md reaches most tools. Claude Code and Gemini CLI read their own files instead, so those
// get the same section when the repo has them.
pub fn instructions_targets(root: &Path, file: Option<&Path>) -> Vec<PathBuf> {
    if let Some(file) = file {
        return vec![root.join(file)];
    }
    let mut targets = vec![root.join("AGENTS.md")];
    targets.extend(
        ["CLAUDE.md", "GEMINI.md"]
            .iter()
            .map(|name| root.join(name))
            .filter(|path| path.exists()),
    );
    targets
}

pub fn install_instructions(target: &Path) -> Result<()> {
    let existing = fs::read_to_string(target).unwrap_or_default();
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let block = format!(
        "{START_MARKER}\n{}\n{END_MARKER}\n",
        INSTRUCTIONS.trim_end()
    )
    .replace('\n', newline);

    let updated = match (existing.find(START_MARKER), existing.find(END_MARKER)) {
        (Some(start), Some(end)) if start < end => {
            let after = end + END_MARKER.len();
            let rest = existing[after..]
                .strip_prefix(newline)
                .unwrap_or(&existing[after..]);
            format!("{}{block}{rest}", &existing[..start])
        }
        _ if existing.is_empty() => block,
        _ => {
            let separator = if existing.ends_with(newline) {
                newline.to_string()
            } else {
                format!("{newline}{newline}")
            };
            format!("{existing}{separator}{block}")
        }
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(target, updated)?;
    Ok(())
}

pub fn install_hook(root: &Path) -> Result<PathBuf> {
    let path = root.join(".claude").join("settings.json");
    let mut settings: Value = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| {
            format!("{} isn't valid JSON, so it wasn't changed", path.display())
        })?,
        Err(_) => json!({}),
    };
    let Some(top) = settings.as_object_mut() else {
        bail!(
            "{} isn't a JSON object, so it wasn't changed",
            path.display()
        );
    };
    let hooks = top.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!(
            "'hooks' in {} isn't an object, so it wasn't changed",
            path.display()
        );
    };
    let pre_tool_use = hooks.entry("PreToolUse").or_insert_with(|| json!([]));
    let Some(entries) = pre_tool_use.as_array_mut() else {
        bail!(
            "'PreToolUse' in {} isn't a list, so it wasn't changed",
            path.display()
        );
    };

    for tool in HOOK_TOOLS {
        let already = entries.iter().any(|entry| {
            entry["matcher"] == *tool
                && entry["hooks"]
                    .as_array()
                    .is_some_and(|hooks| hooks.iter().any(|hook| hook["command"] == HOOK_COMMAND))
        });
        if !already {
            entries.push(json!({
                "matcher": tool,
                "hooks": [{ "type": "command", "command": HOOK_COMMAND, "timeout": 5 }],
            }));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&settings)?),
    )?;
    Ok(path)
}

static SHELL_SEARCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*(?:cd\s+\S+\s*(?:&&|;)\s*)?(?:git\s+grep|grep|rg|egrep)\b((?:\s+-{1,2}[-A-Za-z0-9=]+)*)\s+['"]?([^'"\s|]+)['"]?"#)
        .expect("valid search pattern")
});
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\\b)?([A-Za-z_$][A-Za-z0-9_$]{2,})(?:\\b)?$")
        .expect("valid identifier pattern")
});

fn searched_name(tool: &str, input: &Value) -> Option<String> {
    let pattern = match tool {
        "Grep" => input["pattern"].as_str()?.to_string(),
        "Bash" => {
            let captures = SHELL_SEARCH.captures(input["command"].as_str()?)?;
            captures.get(2)?.as_str().to_string()
        }
        _ => return None,
    };
    let captures = IDENTIFIER.captures(&pattern)?;
    Some(captures.get(1)?.as_str().to_string())
}

fn search_note(index: &Index, name: &str) -> Option<String> {
    let mut places: Vec<String> = index
        .files
        .iter()
        .flat_map(|(path, file)| {
            file.symbols
                .iter()
                .filter(|symbol| {
                    symbol.name == name
                        && !matches!(symbol.kind.as_str(), "heading" | "key" | "section")
                })
                .map(move |symbol| format!("{path}:{}", symbol.line))
        })
        .collect();
    if places.is_empty() {
        return None;
    }
    let total = places.len();
    places.truncate(2);
    let more = if total > 2 {
        format!(" and {} more", total - 2)
    } else {
        String::new()
    };
    Some(format!(
        "Drift: {name} is defined at {}{more}. `drift show {name}` prints it, `drift refs {name}` lists its uses.",
        places.join(", ")
    ))
}

fn read_note(index: &Index, root: &Path, input: &Value) -> Option<String> {
    if !input["offset"].is_null() || !input["limit"].is_null() {
        return None;
    }
    let file_path = PathBuf::from(input["file_path"].as_str()?);
    let size = fs::metadata(&file_path).ok()?.len();
    if size < LARGE_READ_BYTES {
        return None;
    }
    let canonical = file_path.canonicalize().ok()?;
    let canonical_root = root.canonicalize().ok()?;
    let relative = canonical
        .strip_prefix(canonical_root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    index.files.contains_key(&relative).then(|| {
        format!(
            "Drift: this file is {} KB. `drift outline {relative}` lists its definitions with line ranges, so you can read only what you need.",
            size / 1024
        )
    })
}

fn note_for(event: &Value) -> Option<String> {
    let tool = event["tool_name"].as_str()?;
    if !HOOK_TOOLS.contains(&tool) {
        return None;
    }
    let cwd = PathBuf::from(event["cwd"].as_str()?);
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(INDEX_FILE).exists())?
        .to_path_buf();
    let text = fs::read_to_string(root.join(INDEX_FILE)).ok()?;
    let index: Index = serde_json::from_str(&text).ok()?;
    let input = &event["tool_input"];

    if tool == "Read" {
        read_note(&index, &root, input)
    } else {
        search_note(&index, &searched_name(tool, input)?)
    }
}

// Must never get in the agent's way: any failure here means no note, and the tool runs as normal.
pub fn hook() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(event) = serde_json::from_str::<Value>(&input) else {
        return;
    };
    if let Some(note) = note_for(&event) {
        let output = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": note,
            }
        });
        println!("{output}");
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SKILL, portable_skill, searched_name};

    #[test]
    fn the_portable_skill_drops_only_the_claude_specific_field() {
        assert!(SKILL.contains("paths:"));
        let portable = portable_skill();
        assert!(!portable.contains("paths:"));
        assert!(!portable.contains("**/*.ts"));
        assert!(portable.starts_with("---\nname: drift\ndescription: "));
        assert!(portable.contains("---\n\n# Drift"));
        assert!(portable.contains("drift query <symbol>"));
    }

    fn bash(command: &str) -> Option<String> {
        searched_name("Bash", &json!({ "command": command }))
    }

    #[test]
    fn plain_name_searches_are_recognized() {
        assert_eq!(bash("grep -rn loadUser src/").as_deref(), Some("loadUser"));
        assert_eq!(bash("rg 'loadUser'").as_deref(), Some("loadUser"));
        assert_eq!(bash("git grep -n loadUser").as_deref(), Some("loadUser"));
        assert_eq!(bash("cd src && rg loadUser").as_deref(), Some("loadUser"));
        assert_eq!(
            searched_name("Grep", &json!({ "pattern": "\\bloadUser\\b" })).as_deref(),
            Some("loadUser")
        );
    }

    #[test]
    fn other_commands_and_patterns_are_left_alone() {
        assert_eq!(bash("cargo test | grep failed"), None);
        assert_eq!(bash("grep -rn 'load.*User' src"), None);
        assert_eq!(bash("ls"), None);
        assert_eq!(searched_name("Grep", &json!({ "pattern": "a|b" })), None);
        assert_eq!(searched_name("Read", &json!({ "file_path": "x" })), None);
    }
}
