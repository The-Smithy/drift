use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("drift-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Fixture { root }
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn drift(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_drift"))
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn index_json(&self) -> Value {
        read_json(&self.root.join(".drift-index.json"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn gitignore_patterns_do_not_match_as_substrings() {
    let fixture = Fixture::new("gitignore");
    fixture.write(".gitignore", "env\n*.gen.ts\nsecret/\n");
    fixture.write("src/environment.ts", "export const a = 1;\n");
    fixture.write("src/layout.ts", "export const b = 1;\n");
    fixture.write("src/types.gen.ts", "export const c = 1;\n");
    fixture.write("secret/key.ts", "export const d = 1;\n");
    fixture.write("node_modules/dep/index.js", "exports.e = 1;\n");
    fixture.write(".hidden/f.ts", "export const f = 1;\n");
    fixture.write("bundle.min.js", "var g=1;\n");
    fixture.write(".eslintrc.js", "module.exports = {};\n");
    fixture.write("skipme.ts", "export const h = 1;\n");
    fixture.write("huge.ts", &"// padding\n".repeat(60_000));

    assert!(
        fixture
            .drift(&["index", ".", "--ignore-files", "skipme.ts"])
            .status
            .success()
    );

    let index = fixture.index_json();
    let files: Vec<&str> = index["files"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        files,
        vec![".eslintrc.js", "src/environment.ts", "src/layout.ts"]
    );
}

#[test]
fn full_index_keeps_nulls_and_reports_real_test_counts() {
    let fixture = Fixture::new("full");
    fixture.write(
        "a.test.ts",
        "export function helper() {}\n\
         describe('s', () => {\n  it('one', () => {});\n  it('two', () => {});\n});\n",
    );
    fixture.write("package.json", "{\"devDependencies\": {\"jest\": \"1\"}}\n");

    let output = fixture.drift(&["index"]);
    assert!(
        stdout(&output).contains("1 files, 1 symbols, 2 tests"),
        "{}",
        stdout(&output)
    );

    let index = fixture.index_json();
    assert_eq!(index["version"], "1.1");
    assert_eq!(index["tests"]["a.test.ts"]["framework"], "jest");

    let helper = &index["files"]["a.test.ts"]["symbols"][0];
    assert_eq!(helper["name"], "helper");
    assert_eq!(helper["end_line"], 1);
    assert!(helper["parent"].is_null());
    assert_eq!(helper["async"], false);
    assert_eq!(index["symbol_index"]["helper"][0]["exported"], true);
}

#[test]
fn compact_index_strips_defaults_and_private_symbols_from_lookup() {
    let fixture = Fixture::new("compact");
    fixture.write(
        "a.ts",
        "export function pub1() {}\nfunction priv1() {}\nimport './side';\n",
    );

    assert!(fixture.drift(&["index", "--compact"]).status.success());

    let raw = fs::read_to_string(fixture.root.join(".drift-index.json")).unwrap();
    assert_eq!(raw.lines().count(), 1);

    let index = fixture.index_json();
    assert!(index["symbol_index"]["pub1"].is_array());
    assert!(index["symbol_index"].get("priv1").is_none());

    let symbols = index["files"]["a.ts"]["symbols"].as_array().unwrap();
    assert_eq!(symbols.len(), 2);
    let private = symbols.iter().find(|s| s["name"] == "priv1").unwrap();
    for absent in ["exported", "async", "parent"] {
        assert!(private.get(absent).is_none(), "{absent}");
    }
    assert!(index["files"]["a.ts"]["imports"][0].get("names").is_none());
}

#[test]
fn query_works_on_full_and_compact_indexes() {
    let fixture = Fixture::new("query");
    fixture.write(
        "src/user.ts",
        "export class UserController {\n  load() {}\n}\nfunction hiddenHelper() {}\n",
    );

    for args in [&["index"][..], &["index", "--compact"][..]] {
        assert!(fixture.drift(args).status.success());

        let exact = stdout(&fixture.drift(&["query", "UserController"]));
        assert!(exact.contains("class"), "{exact}");
        assert!(exact.contains("src/user.ts:1-3 (exported)"), "{exact}");

        let method = stdout(&fixture.drift(&["query", "load"]));
        assert!(
            method.contains("src/user.ts:2 (exported) in UserController"),
            "{method}"
        );

        let private = stdout(&fixture.drift(&["query", "hiddenHelper"]));
        assert!(private.contains("src/user.ts:4"), "{private}");

        let fuzzy = stdout(&fixture.drift(&["query", "usercontrol"]));
        assert!(fuzzy.contains("UserController"), "{fuzzy}");

        let missing = fixture.drift(&["query", "nope"]);
        assert_eq!(missing.status.code(), Some(1));
    }
}

#[test]
fn stats_reads_compact_indexes() {
    let fixture = Fixture::new("stats");
    fixture.write("a.py", "def test_a():\n    pass\n\ndef b():\n    pass\n");
    assert!(fixture.drift(&["index", "--compact"]).status.success());

    let output = stdout(&fixture.drift(&["stats"]));
    assert!(output.contains("Symbols:    2 (2 unique)"), "{output}");
    assert!(output.contains("Tests:      1 across 1 files"), "{output}");
    assert!(output.contains("python"), "{output}");
}

#[test]
fn install_writes_a_discoverable_skill() {
    let fixture = Fixture::new("install");
    assert!(fixture.drift(&["install"]).status.success());

    let skill = fs::read_to_string(fixture.root.join(".claude/skills/drift/SKILL.md")).unwrap();
    assert!(skill.starts_with("---\nname: drift\n") || skill.starts_with("---\r\nname: drift\r\n"));
    assert!(skill.contains("paths:"));
    assert!(!skill.contains("globs:"));
}

#[test]
fn batch_indexes_only_git_repos() {
    let fixture = Fixture::new("batch");
    fixture.write("repo_a/.git/HEAD", "ref: refs/heads/master\n");
    fixture.write("repo_a/a.rs", "pub fn a() {}\n");
    fixture.write("not_a_repo/b.rs", "pub fn b() {}\n");

    let output = fixture.drift(&["batch", ".", "--compact", "--install"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout(&output).contains("Found 1 repos"));
    assert!(fixture.root.join("repo_a/.drift-index.json").exists());
    assert!(
        fixture
            .root
            .join("repo_a/.claude/skills/drift/SKILL.md")
            .exists()
    );
    assert!(!fixture.root.join("not_a_repo/.drift-index.json").exists());
}

#[test]
fn non_utf8_and_bom_files_still_index() {
    let fixture = Fixture::new("encoding");
    fs::write(
        fixture.root.join("latin.py"),
        b"# caf\xe9\ndef kept():\n    pass\n",
    )
    .unwrap();
    fixture.write("bom.ts", "\u{feff}export function first() {}\n");
    let mut utf16 = vec![0xFF, 0xFE];
    for unit in "export type Json = string;\n".encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    fs::write(fixture.root.join("types_db.ts"), utf16).unwrap();

    assert!(fixture.drift(&["index"]).status.success());
    let index = fixture.index_json();
    assert_eq!(index["symbol_index"]["kept"][0]["line"], 2);
    assert_eq!(index["symbol_index"]["first"][0]["line"], 1);
    assert_eq!(index["symbol_index"]["Json"][0]["kind"], "type");
}

#[test]
fn missing_index_fails_with_a_clear_message() {
    let fixture = Fixture::new("missing");
    let output = fixture.drift(&["query", "x"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("drift index"));
}
