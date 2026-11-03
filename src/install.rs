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

pub fn install_skill(root: &Path) -> Result<PathBuf> {
    let directory = root.join(".claude").join("skills").join("drift");
    fs::create_dir_all(&directory)?;
    let skill = directory.join("SKILL.md");
    fs::write(&skill, SKILL)?;
    Ok(skill)
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

pub fn instructions_target(root: &Path, file: Option<&Path>) -> PathBuf {
    if let Some(file) = file {
        return root.join(file);
    }
    ["CLAUDE.md", "AGENTS.md"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| root.join("AGENTS.md"))
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
                .filter(|symbol| symbol.name == name)
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

    use super::searched_name;

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
