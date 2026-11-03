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
            .env("DRIFT_NO_UPDATE_CHECK", "1")
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
    assert_eq!(index["version"], "1.3");
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

fn lookup_fixture(name: &str) -> Fixture {
    let fixture = Fixture::new(name);
    fixture.write(
        "src/lib/auth.ts",
        "import { sign } from 'jsonwebtoken';\n\
         export class Auth {\n  check() {}\n}\n\
         function helper() {}\n",
    );
    fixture.write("src/lib/oauth.ts", "export const o = 1;\n");
    fixture.write(
        "src/app/page.ts",
        "import { Auth } from '../lib/auth';\nimport * as all from '@/lib/oauth';\n",
    );
    fixture.write("src/admin/auth.ts", "import '../lib/oauth';\n");
    fixture.write(
        "tests/auth.test.ts",
        "describe('login', () => {\n  it('accepts a token', () => {});\n  it('rejects a bad one', () => {});\n});\n\
         describe('logout', () => {\n  it('clears the session', () => {});\n});\n",
    );
    assert!(fixture.drift(&["index", ".", "--compact"]).status.success());
    fixture
}

#[test]
fn outline_lists_symbols_and_imports() {
    let fixture = lookup_fixture("outline");

    let output = stdout(&fixture.drift(&["outline", "src/lib/auth.ts"]));
    assert!(
        output.contains("src/lib/auth.ts  typescript, 6 lines"),
        "{output}"
    );
    assert!(output.contains("Auth  2-4 (exported)"), "{output}");
    assert!(output.contains("check  3 (exported) in Auth"), "{output}");
    assert!(
        output.contains("helper  5\n") || output.contains("helper  5\r\n"),
        "{output}"
    );
    assert!(output.contains("jsonwebtoken: sign"), "{output}");

    let by_suffix = stdout(&fixture.drift(&["outline", "lib/auth.ts"]));
    assert!(
        by_suffix.contains("src/lib/auth.ts  typescript"),
        "{by_suffix}"
    );

    let canonical = fs::canonicalize(&fixture.root).unwrap();
    let root_text = canonical.to_string_lossy();
    let root_text = root_text.strip_prefix(r"\\?\").unwrap_or(&root_text);
    let absolute = Path::new(root_text).join("src").join("lib").join("auth.ts");
    let output = stdout(&fixture.drift(&["outline", absolute.to_str().unwrap()]));
    assert!(output.contains("src/lib/auth.ts  typescript"), "{output}");
}

#[test]
fn outline_reports_ambiguous_and_unknown_files() {
    let fixture = lookup_fixture("outline-ambiguous");

    let ambiguous = fixture.drift(&["outline", "auth.ts"]);
    assert_eq!(ambiguous.status.code(), Some(1));
    let output = stdout(&ambiguous);
    assert!(
        output.contains("src/lib/auth.ts") && output.contains("src/admin/auth.ts"),
        "{output}"
    );
    assert!(!output.contains("oauth"), "{output}");

    let unknown = fixture.drift(&["outline", "nope.ts"]);
    assert_eq!(unknown.status.code(), Some(1));
}

#[test]
fn tests_summarize_and_filter() {
    let fixture = lookup_fixture("tests");

    let summary = stdout(&fixture.drift(&["tests"]));
    assert!(
        summary.contains("tests/auth.test.ts  unknown  3 tests"),
        "{summary}"
    );
    assert!(summary.contains("3 tests across 1 file"), "{summary}");

    let by_case = stdout(&fixture.drift(&["tests", "TOKEN"]));
    assert!(by_case.contains("login"), "{by_case}");
    assert!(by_case.contains("2  accepts a token"), "{by_case}");
    assert!(
        !by_case.contains("rejects") && !by_case.contains("logout"),
        "{by_case}"
    );

    let by_suite = stdout(&fixture.drift(&["tests", "logout"]));
    assert!(by_suite.contains("clears the session"), "{by_suite}");
    assert!(!by_suite.contains("accepts"), "{by_suite}");

    let by_path = stdout(&fixture.drift(&["tests", "auth.test"]));
    assert!(
        by_path.contains("accepts") && by_path.contains("clears"),
        "{by_path}"
    );

    assert_eq!(fixture.drift(&["tests", "nope"]).status.code(), Some(1));
}

#[test]
fn importers_resolve_paths_and_match_text() {
    let fixture = lookup_fixture("importers");

    let by_path = stdout(&fixture.drift(&["importers", "src/lib/auth.ts"]));
    assert!(
        by_path.contains("src/app/page.ts  ../lib/auth: Auth"),
        "{by_path}"
    );
    assert!(!by_path.contains("oauth"), "{by_path}");

    let oauth = stdout(&fixture.drift(&["importers", "src/lib/oauth.ts"]));
    assert!(oauth.contains("src/app/page.ts  @/lib/oauth: *"), "{oauth}");
    assert!(oauth.contains("src/admin/auth.ts  ../lib/oauth"), "{oauth}");

    let by_text = stdout(&fixture.drift(&["importers", "jsonwebtoken"]));
    assert!(
        by_text.contains("src/lib/auth.ts  jsonwebtoken: sign"),
        "{by_text}"
    );

    assert_eq!(fixture.drift(&["importers", "nope"]).status.code(), Some(1));
}

#[test]
fn importers_resolve_rust_and_python_modules() {
    let fixture = Fixture::new("importers-rust");
    fixture.write("src/main.rs", "mod model;\nmod other;\nuse std::fmt;\n");
    fixture.write("src/model.rs", "pub struct Index;\n");
    fixture.write(
        "src/other.rs",
        "use crate::model::Index;\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n",
    );
    fixture.write("src/unrelated.rs", "use std::collections::HashMap;\n");
    fixture.write("pkg/mod_a.py", "from . import mod_b\n");
    fixture.write("pkg/mod_b.py", "VALUE = 1\n");
    assert!(fixture.drift(&["index", ".", "--compact"]).status.success());

    let output = stdout(&fixture.drift(&["importers", "src/model.rs"]));
    assert!(output.contains("src/main.rs  mod model"), "{output}");
    assert!(
        output.contains("src/other.rs  crate::model: Index"),
        "{output}"
    );
    assert!(
        !output.contains("unrelated") && !output.contains("std"),
        "{output}"
    );

    let by_suffix = stdout(&fixture.drift(&["importers", "model.rs"]));
    assert_eq!(by_suffix, output);

    let python = stdout(&fixture.drift(&["importers", "pkg/mod_b.py"]));
    assert!(python.contains("pkg/mod_a.py  .: mod_b"), "{python}");
}

fn feature_fixture(name: &str) -> Fixture {
    let fixture = Fixture::new(name);
    fixture.write(".git/HEAD", "ref: refs/heads/master\n");
    fixture.write(
        "src/auth.ts",
        "/** Checks a session token. */\n\
         export function verifyToken(token: string): boolean {\n  return token.length > 0;\n}\n\n\
         export class SessionStore {\n  load(id: string): string {\n    return verifyToken(id) ? id : '';\n  }\n}\n\n\
         function hiddenHelper() {}\n",
    );
    fixture.write(
        "src/app.ts",
        "import { verifyToken } from './auth';\n\
         // verifyToken in a comment\n\
         const note = 'verifyToken in a string';\n\
         export function handleRequest(token: string) {\n  return verifyToken(token);\n}\n",
    );
    fixture.write(
        "tests/auth.test.ts",
        "import { verifyToken } from '../src/auth';\n\
         test('accepts a token', () => {\n  verifyToken('x');\n});\n",
    );
    fixture.write(
        "supabase/migrations/001_a.sql",
        "CREATE FUNCTION verifyToken() RETURNS int AS $$ SELECT 1; $$ LANGUAGE sql;\n",
    );
    fixture.write(
        "supabase/migrations/002_b.sql",
        "CREATE FUNCTION verifyToken() RETURNS int AS $$ SELECT 2; $$ LANGUAGE sql;\n",
    );
    fixture
}

#[test]
fn lookups_build_the_index_on_first_use_and_refresh_when_stale() {
    let fixture = feature_fixture("freshness");
    assert!(!fixture.root.join(".drift-index.json").exists());

    let first = stdout(&fixture.drift(&["query", "verifyToken"]));
    assert!(first.contains("src/auth.ts:2-4"), "{first}");
    assert!(fixture.root.join(".drift-index.json").exists());
    let exclude = fs::read_to_string(fixture.root.join(".git/info/exclude")).unwrap_or_default();
    assert!(exclude.is_empty() || exclude.contains(".drift-index.json"));

    // A new definition, a moved line and a deleted file must all show up with no reindex.
    fixture.write("src/new.ts", "export function brandNew() {}\n");
    assert!(stdout(&fixture.drift(&["query", "brandNew"])).contains("src/new.ts:1"));
    fixture.write("src/new.ts", "\n\nexport function brandNew() {}\n");
    assert!(stdout(&fixture.drift(&["query", "brandNew"])).contains("src/new.ts:3"));
    fs::remove_file(fixture.root.join("src/new.ts")).unwrap();
    assert_eq!(fixture.drift(&["query", "brandNew"]).status.code(), Some(1));
}

#[test]
fn lookups_work_from_a_subfolder() {
    let fixture = feature_fixture("subfolder");
    let output = Command::new(env!("CARGO_BIN_EXE_drift"))
        .args(["query", "verifyToken"])
        .current_dir(fixture.root.join("src"))
        .env("DRIFT_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(
        stdout(&output).contains("src/auth.ts:2-4"),
        "{}",
        stdout(&output)
    );
}

#[test]
fn an_explicit_index_is_used_as_is() {
    let fixture = feature_fixture("explicit");
    assert!(
        fixture
            .drift(&["index", ".", "-o", "saved.json"])
            .status
            .success()
    );
    fixture.write("src/new.ts", "export function brandNew() {}\n");
    let output = fixture.drift(&["query", "brandNew", "--index", "saved.json"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn output_is_capped_and_says_how_to_see_more() {
    let fixture = Fixture::new("budget");
    fixture.write(".git/HEAD", "ref: refs/heads/master\n");
    let many: String = (0..2000)
        .map(|i| format!("export function handler{i}() {{}}\n"))
        .collect();
    fixture.write("src/a.ts", &many);

    let capped = stdout(&fixture.drift(&["query", "handler"]));
    assert!(capped.len() < 4200, "{}", capped.len());
    assert!(capped.contains("more lines"), "{capped}");

    let limited = stdout(&fixture.drift(&["query", "handler", "--limit", "3"]));
    assert_eq!(limited.lines().count(), 4);

    let everything = stdout(&fixture.drift(&["query", "handler", "--all"]));
    assert_eq!(everything.lines().count(), 2000);
}

#[test]
fn a_closed_pipe_is_not_a_crash() {
    use std::process::Stdio;
    let fixture = Fixture::new("pipe");
    fixture.write(".git/HEAD", "ref: refs/heads/master\n");
    let many: String = (0..20000)
        .map(|i| format!("export function handler{i}() {{}}\n"))
        .collect();
    fixture.write("src/a.ts", &many);
    assert!(fixture.drift(&["index"]).status.success());

    let mut child = Command::new(env!("CARGO_BIN_EXE_drift"))
        .args(["query", "handler", "--all"])
        .current_dir(&fixture.root)
        .env("DRIFT_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn results_rank_source_first_and_newest_migration_before_older() {
    let fixture = feature_fixture("ranking");
    let output = stdout(&fixture.drift(&["query", "verifyToken"]));
    let lines: Vec<&str> = output.lines().collect();
    assert!(lines[0].contains("src/auth.ts"), "{output}");
    assert!(lines[1].contains("002_b.sql"), "{output}");
    assert!(lines[2].contains("001_a.sql"), "{output}");

    let scoped = stdout(&fixture.drift(&["query", "verifyToken", "--in", "supabase"]));
    assert!(!scoped.contains("src/auth.ts"), "{scoped}");
}

#[test]
fn query_takes_several_names_and_suggests_on_a_miss() {
    let fixture = feature_fixture("multi");
    let output = fixture.drift(&["query", "handleRequest", "verifyTokn"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("handleRequest:"), "{text}");
    assert!(text.contains("src/app.ts:4-6"), "{text}");

    let miss = fixture.drift(&["query", "verifyTokn"]);
    assert_eq!(miss.status.code(), Some(1));
    let text = stdout(&miss);
    assert!(text.contains("Did you mean: verifyToken?"), "{text}");
}

#[test]
fn show_prints_only_the_definition() {
    let fixture = feature_fixture("show");
    let output = stdout(&fixture.drift(&["show", "src/auth.ts:verifyToken"]));
    assert!(output.contains("return token.length > 0;"), "{output}");
    assert!(!output.contains("SessionStore"), "{output}");

    let method = stdout(&fixture.drift(&["show", "SessionStore.load"]));
    assert!(method.contains("load(id: string)"), "{method}");
    assert!(!method.contains("export function"), "{method}");

    let several = stdout(&fixture.drift(&["show", "verifyToken"]));
    assert!(several.contains("return token.length > 0;"), "{several}");
    assert!(several.contains("Also defined at"), "{several}");

    assert_eq!(fixture.drift(&["show", "nope"]).status.code(), Some(1));
}

#[test]
fn sig_prints_the_signature_and_doc_without_the_body() {
    let fixture = feature_fixture("sig");
    let output = stdout(&fixture.drift(&["sig", "src/auth.ts:verifyToken"]));
    assert!(
        output.contains("function verifyToken(token: string): boolean"),
        "{output}"
    );
    assert!(output.contains("Checks a session token."), "{output}");
    assert!(!output.contains("token.length"), "{output}");
}

#[test]
fn refs_find_uses_by_enclosing_definition_and_skip_noise() {
    let fixture = feature_fixture("refs");
    let output = stdout(&fixture.drift(&["refs", "verifyToken"]));
    assert!(output.contains("src/app.ts"), "{output}");
    assert!(
        output.contains("5 handleRequest: return verifyToken(token);"),
        "{output}"
    );
    assert!(output.contains("8 load: "), "{output}");
    assert!(!output.contains("in a comment"), "{output}");
    assert!(!output.contains("in a string"), "{output}");
    assert!(!output.contains("import {"), "{output}");
    assert!(
        output.find("src/app.ts").unwrap() < output.find("tests/auth.test.ts").unwrap(),
        "{output}"
    );

    let unused = fixture.drift(&["refs", "hiddenHelper"]);
    assert_eq!(unused.status.code(), Some(1));
    assert!(stdout(&unused).contains("No references"));
}

#[test]
fn find_matches_words_in_any_order() {
    let fixture = feature_fixture("find");
    let output = stdout(&fixture.drift(&["find", "token", "verify"]));
    assert!(
        output.lines().next().unwrap().contains("verifyToken"),
        "{output}"
    );
    let by_doc = stdout(&fixture.drift(&["find", "session", "token"]));
    assert!(by_doc.contains("verifyToken"), "{by_doc}");
    assert_eq!(
        fixture.drift(&["find", "nothing", "here"]).status.code(),
        Some(1)
    );
}

#[test]
fn tests_for_finds_tests_through_imports_and_names() {
    let fixture = feature_fixture("tests-for");
    let output = stdout(&fixture.drift(&["tests", "--for", "src/auth.ts"]));
    assert!(output.contains("tests/auth.test.ts"), "{output}");
    assert_eq!(
        fixture
            .drift(&["tests", "--for", "src/app.ts"])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn context_bundles_what_is_needed_before_a_change() {
    let fixture = feature_fixture("context");
    let symbol = stdout(&fixture.drift(&["context", "verifyToken"]));
    assert!(
        symbol.contains("function verifyToken(token: string): boolean"),
        "{symbol}"
    );
    assert!(symbol.contains("Used in"), "{symbol}");
    assert!(symbol.contains("src/app.ts:5 handleRequest"), "{symbol}");
    assert!(symbol.contains("Imported by"), "{symbol}");
    assert!(symbol.contains("tests/auth.test.ts"), "{symbol}");

    let file = stdout(&fixture.drift(&["context", "src/auth.ts"]));
    assert!(file.contains("src/auth.ts  typescript"), "{file}");
    assert!(file.contains("class SessionStore"), "{file}");
    assert!(!file.contains("hiddenHelper"), "{file}");
}

#[test]
fn outline_can_show_signatures() {
    let fixture = feature_fixture("outline-sig");
    let output = stdout(&fixture.drift(&["outline", "src/auth.ts", "--sig"]));
    assert!(
        output.contains("function verifyToken(token: string): boolean  2-4"),
        "{output}"
    );
}

#[test]
fn map_summarizes_the_repo() {
    let fixture = feature_fixture("map");
    let output = stdout(&fixture.drift(&["map"]));
    assert!(output.contains("Languages: "), "{output}");
    assert!(output.contains("Most imported:"), "{output}");
    assert!(
        output.contains("src/auth.ts  2 importers: verifyToken, SessionStore"),
        "{output}"
    );
    assert!(output.contains("Tests: 1 file"), "{output}");
}

fn git(fixture: &Fixture, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .current_dir(&fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn changes_map_a_diff_to_definitions() {
    let fixture = Fixture::new("changes");
    git(&fixture, &["init", "-q"]);
    fixture.write(
        "a.ts",
        "export function first() {\n  return 1;\n}\n\nexport function second() {\n  return 2;\n}\n",
    );
    fixture.write("gone.ts", "export const g = 1;\n");
    git(&fixture, &["add", "."]);
    git(&fixture, &["commit", "-qm", "start"]);

    assert!(stdout(&fixture.drift(&["changes"])).contains("No changes"));

    fixture.write(
        "a.ts",
        "export function first() {\n  return 1;\n}\n\nexport function second() {\n  return 3;\n}\n",
    );
    fs::remove_file(fixture.root.join("gone.ts")).unwrap();
    fixture.write("fresh.ts", "export const f = 1;\n");

    let output = stdout(&fixture.drift(&["changes"]));
    assert!(
        output.contains("function second  5-7 (exported)"),
        "{output}"
    );
    assert!(!output.contains("function first"), "{output}");
    assert!(output.contains("gone.ts  deleted"), "{output}");
    assert!(output.contains("fresh.ts  new, untracked"), "{output}");

    assert!(
        !fixture
            .drift(&["changes", "--", "--output=x"])
            .status
            .success()
    );
}

#[test]
fn bench_is_repeatable_and_reports_savings() {
    let fixture = feature_fixture("bench");
    let padding: String = (0..300).map(|i| format!("// padding line {i}\n")).collect();
    fixture.write(
        "src/large.ts",
        &format!("{padding}export function bigOne() {{\n  return 1;\n}}\nexport const a = 1;\nexport const b = 2;\n"),
    );

    let first = fixture.drift(&["bench", ".", "--json"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = fixture.drift(&["bench", ".", "--json"]);
    assert_eq!(stdout(&first), stdout(&second));

    let report: Value = serde_json::from_str(&stdout(&first)).unwrap();
    assert!(
        report["total"]["saved_percent"].as_f64().unwrap() > 0.0,
        "{report}"
    );
    let tasks: Vec<&str> = report["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind["task"].as_str().unwrap())
        .collect();
    for expected in [
        "find a definition",
        "read one function",
        "understand a file",
        "find uses",
    ] {
        assert!(tasks.contains(&expected), "{tasks:?}");
    }

    fixture.write("before.json", &stdout(&first));
    let compared = stdout(&fixture.drift(&["bench", ".", "--compare", "before.json"]));
    assert!(compared.contains("vs before"), "{compared}");

    assert!(
        fixture
            .drift(&["bench", ".", "--check", "1"])
            .status
            .success()
    );
    assert_eq!(
        fixture
            .drift(&["bench", ".", "--check", "100"])
            .status
            .code(),
        Some(1)
    );
}

fn run_hook(fixture: &Fixture, event: Value) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_drift"))
        .arg("hook")
        .current_dir(&fixture.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(event.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    stdout(&output)
}

#[test]
fn hook_adds_notes_without_blocking_and_fails_open() {
    let fixture = feature_fixture("hook");
    assert!(fixture.drift(&["index", ".", "--compact"]).status.success());
    let cwd = fixture.root.to_string_lossy().into_owned();

    let note = run_hook(
        &fixture,
        serde_json::json!({ "tool_name": "Bash", "cwd": cwd, "tool_input": { "command": "grep -rn verifyToken src" } }),
    );
    let parsed: Value = serde_json::from_str(&note).unwrap();
    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let context = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("src/auth.ts:2"), "{context}");
    assert!(
        parsed["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none()
    );

    for quiet in [
        serde_json::json!({ "tool_name": "Bash", "cwd": cwd, "tool_input": { "command": "cargo test | grep failed" } }),
        serde_json::json!({ "tool_name": "Grep", "cwd": cwd, "tool_input": { "pattern": "unknownName" } }),
        serde_json::json!({ "tool_name": "Edit", "cwd": cwd, "tool_input": {} }),
        serde_json::json!({ "not": "an event" }),
    ] {
        assert_eq!(run_hook(&fixture, quiet), "");
    }

    let padding = "x".repeat(10_000);
    fixture.write("src/big.ts", &format!("export const big = '{padding}';\n"));
    assert!(fixture.drift(&["index", ".", "--compact"]).status.success());
    let big = fixture
        .root
        .join("src/big.ts")
        .to_string_lossy()
        .into_owned();
    let read = run_hook(
        &fixture,
        serde_json::json!({ "tool_name": "Read", "cwd": cwd, "tool_input": { "file_path": big } }),
    );
    assert!(read.contains("drift outline src/big.ts"), "{read}");
    let ranged = run_hook(
        &fixture,
        serde_json::json!({ "tool_name": "Read", "cwd": cwd, "tool_input": { "file_path": big, "limit": 5 } }),
    );
    assert_eq!(ranged, "");
}

#[test]
fn hook_install_merges_settings_and_is_idempotent() {
    let fixture = Fixture::new("hook-install");
    fixture.write(
        ".claude/settings.json",
        "{\n  \"theme\": \"dark\",\n  \"hooks\": { \"PreToolUse\": [{ \"matcher\": \"Edit\", \"hooks\": [] }] }\n}\n",
    );
    assert!(fixture.drift(&["install", ".", "--hook"]).status.success());
    assert!(fixture.drift(&["install", ".", "--hook"]).status.success());

    let settings = read_json(&fixture.root.join(".claude/settings.json"));
    assert_eq!(settings["theme"], "dark");
    let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0]["matcher"], "Edit");
    let matchers: Vec<&str> = entries[1..]
        .iter()
        .map(|entry| entry["matcher"].as_str().unwrap())
        .collect();
    assert_eq!(matchers, ["Bash", "Grep", "Read"]);

    let broken = Fixture::new("hook-broken");
    broken.write(".claude/settings.json", "{ not json");
    assert!(!broken.drift(&["install", ".", "--hook"]).status.success());
    assert_eq!(
        fs::read_to_string(broken.root.join(".claude/settings.json")).unwrap(),
        "{ not json"
    );
}

#[test]
fn instructions_install_updates_in_place() {
    let fixture = Fixture::new("instructions");
    fixture.write("CLAUDE.md", "# Project\n\nKeep this.\n");
    assert!(
        fixture
            .drift(&["install", ".", "--instructions"])
            .status
            .success()
    );
    assert!(
        fixture
            .drift(&["install", ".", "--instructions"])
            .status
            .success()
    );

    let text = fs::read_to_string(fixture.root.join("CLAUDE.md")).unwrap();
    assert!(text.starts_with("# Project\n\nKeep this.\n"), "{text}");
    assert_eq!(text.matches("drift:start").count(), 1, "{text}");
    assert!(text.contains("drift query"), "{text}");

    assert!(
        fixture
            .drift(&["install", ".", "--instructions", "--file", "SITE.md"])
            .status
            .success()
    );
    assert!(fixture.root.join("SITE.md").exists());

    let gitignore = fs::read_to_string(fixture.root.join(".gitignore")).unwrap();
    assert_eq!(gitignore.matches(".drift-index.json").count(), 1);
}

#[test]
fn install_writes_a_skill_for_every_tool_that_reads_skills() {
    let fixture = Fixture::new("install-portable");
    assert!(fixture.drift(&["install"]).status.success());

    let claude = fs::read_to_string(fixture.root.join(".claude/skills/drift/SKILL.md")).unwrap();
    assert!(claude.contains("paths:"), "{claude}");

    let shared = fs::read_to_string(fixture.root.join(".agents/skills/drift/SKILL.md")).unwrap();
    assert!(shared.contains("name: drift"), "{shared}");
    assert!(shared.contains("description: "), "{shared}");
    assert!(!shared.contains("paths:"), "{shared}");

    // Nothing for tools this repo doesn't use.
    for absent in [
        ".kiro",
        ".junie",
        ".continue",
        "CONVENTIONS.md",
        "AGENTS.md",
    ] {
        assert!(!fixture.root.join(absent).exists(), "{absent}");
    }
}

#[test]
fn install_sets_up_tools_the_repo_already_uses() {
    let fixture = Fixture::new("install-detect");
    fixture.write(".kiro/settings.json", "{}");
    fixture.write(".continue/config.yaml", "name: x\n");
    assert!(fixture.drift(&["install"]).status.success());

    assert!(fixture.root.join(".kiro/skills/drift/SKILL.md").exists());
    let rule = fs::read_to_string(fixture.root.join(".continue/rules/drift.md")).unwrap();
    assert!(
        rule.starts_with("---\nname: Drift\nalwaysApply: true\n---\n"),
        "{rule}"
    );
    assert!(rule.contains("drift query"), "{rule}");
    assert!(!fixture.root.join(".junie").exists());
    assert!(!fixture.root.join("CONVENTIONS.md").exists());
}

#[test]
fn install_agent_flag_sets_up_named_tools() {
    let fixture = Fixture::new("install-agent");
    let output = fixture.drift(&["install", ".", "--agent", "junie,cursor"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("cursor: covered by the shared files"));
    assert!(fixture.root.join(".junie/skills/drift/SKILL.md").exists());
    assert!(!fixture.root.join(".kiro").exists());
    assert!(!fixture.root.join(".cursor").exists());

    let all = Fixture::new("install-agent-all");
    assert!(
        all.drift(&["install", ".", "--agent", "all"])
            .status
            .success()
    );
    for expected in [
        ".kiro/skills/drift/SKILL.md",
        ".junie/skills/drift/SKILL.md",
        ".continue/rules/drift.md",
        "CONVENTIONS.md",
        ".aider.conf.yml",
    ] {
        assert!(all.root.join(expected).exists(), "{expected}");
    }

    let unknown = fixture.drift(&["install", ".", "--agent", "nonsense"]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("Known tools"));
}

#[test]
fn aider_config_is_added_to_but_never_rewritten() {
    let fresh = Fixture::new("aider-fresh");
    fresh.write(".aider.conf.yml", "model: x");
    assert!(fresh.drift(&["install"]).status.success());
    assert!(fresh.drift(&["install"]).status.success());
    let text = fs::read_to_string(fresh.root.join(".aider.conf.yml")).unwrap();
    assert_eq!(text, "model: x\nread: CONVENTIONS.md\n");

    let existing = Fixture::new("aider-existing");
    existing.write(".aider.conf.yml", "read: [NOTES.md]\n");
    let output = stdout(&existing.drift(&["install"]));
    assert!(output.contains("add CONVENTIONS.md to `read:`"), "{output}");
    assert_eq!(
        fs::read_to_string(existing.root.join(".aider.conf.yml")).unwrap(),
        "read: [NOTES.md]\n"
    );
}

#[test]
fn instructions_reach_agents_md_and_each_tools_own_file() {
    let fixture = Fixture::new("instructions-targets");
    assert!(
        fixture
            .drift(&["install", ".", "--instructions"])
            .status
            .success()
    );
    assert!(fixture.root.join("AGENTS.md").exists());
    assert!(!fixture.root.join("CLAUDE.md").exists());
    assert!(!fixture.root.join("GEMINI.md").exists());

    fixture.write("CLAUDE.md", "# Claude\n");
    fixture.write("GEMINI.md", "# Gemini\n");
    assert!(
        fixture
            .drift(&["install", ".", "--instructions"])
            .status
            .success()
    );
    for file in ["AGENTS.md", "CLAUDE.md", "GEMINI.md"] {
        let text = fs::read_to_string(fixture.root.join(file)).unwrap();
        assert_eq!(text.matches("drift:start").count(), 1, "{file}");
    }

    let only_one = Fixture::new("instructions-file");
    only_one.write("CLAUDE.md", "# Claude\n");
    assert!(
        only_one
            .drift(&["install", ".", "--instructions", "--file", "SITE.md"])
            .status
            .success()
    );
    assert!(only_one.root.join("SITE.md").exists());
    assert!(!only_one.root.join("AGENTS.md").exists());
    assert!(
        !fs::read_to_string(only_one.root.join("CLAUDE.md"))
            .unwrap()
            .contains("drift:start")
    );
}

#[test]
fn instructions_stay_small_because_they_cost_tokens_every_session() {
    assert!(include_str!("../src/skill.md").len() <= 1800);
    assert!(include_str!("../src/instructions.md").len() <= 900);
}

#[test]
fn skill_only_recommends_commands_that_exist() {
    let skill = include_str!("../src/skill.md");
    for command in [
        "drift query",
        "drift outline",
        "drift tests",
        "drift importers",
    ] {
        assert!(skill.contains(command), "{command}");
    }
    assert!(!skill.contains("jq "));
}

#[test]
fn missing_index_fails_with_a_clear_message() {
    let fixture = Fixture::new("missing");
    let output = fixture.drift(&["query", "x"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("drift index"));
}

// The version check, against a local repo so these never touch the network.
struct UpdateFixture {
    fixture: Fixture,
    cache: PathBuf,
}

impl UpdateFixture {
    fn new(name: &str, tags: &[&str]) -> Self {
        let fixture = Fixture::new(name);
        fixture.write("a.py", "def a():\n    pass\n");
        assert!(fixture.drift(&["index", "."]).status.success());
        let cache = fixture.root.join("cache");
        let remote = fixture.root.join("remote");
        fs::create_dir_all(&remote).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&remote)
                .output()
                .unwrap();
            assert!(status.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "first"]);
        for tag in tags {
            git(&["tag", tag]);
        }
        UpdateFixture { fixture, cache }
    }

    fn run(&self, args: &[&str], enabled: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_drift"));
        command
            .args(args)
            .current_dir(&self.fixture.root)
            .env_remove("CI")
            .env_remove("DRIFT_NO_UPDATE_CHECK")
            .env("DRIFT_CACHE_DIR", &self.cache)
            .env("DRIFT_UPDATE_REMOTE", self.fixture.root.join("remote"));
        if !enabled {
            command.env("DRIFT_NO_UPDATE_CHECK", "1");
        }
        command.output().unwrap()
    }

    fn state_path(&self) -> PathBuf {
        self.cache.join("update-check.json")
    }

    fn seed(&self, latest: &str) {
        fs::create_dir_all(&self.cache).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        fs::write(
            self.state_path(),
            format!(r#"{{"latest":"{latest}","checked_at":{now},"notified_at":0}}"#),
        )
        .unwrap();
    }
}

#[test]
fn notice_goes_to_stderr_once_a_day_and_leaves_stdout_alone() {
    let f = UpdateFixture::new("update-notice", &[]);
    f.seed("99.0.0");

    let first = f.run(&["query", "a"], true);
    assert!(first.status.success(), "{first:?}");
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(stderr.contains("drift 99.0.0 is available"), "{stderr}");
    assert_eq!(stderr.trim().lines().count(), 1);
    assert!(!String::from_utf8_lossy(&first.stdout).contains("available"));

    let second = f.run(&["query", "a"], true);
    assert!(!String::from_utf8_lossy(&second.stderr).contains("available"));
}

#[test]
fn no_notice_when_up_to_date_or_turned_off() {
    let f = UpdateFixture::new("update-quiet", &[]);
    f.seed("0.0.1");
    let older = f.run(&["query", "a"], true);
    assert!(!String::from_utf8_lossy(&older.stderr).contains("available"));

    f.seed("99.0.0");
    let off = f.run(&["query", "a"], false);
    assert!(!String::from_utf8_lossy(&off.stderr).contains("available"));
}

#[test]
fn a_lookup_starts_a_background_check_that_finds_the_newest_release() {
    let f = UpdateFixture::new(
        "update-background",
        &["v98.0.0", "v99.0.0", "v100.0.0-rc.1"],
    );
    assert!(f.run(&["query", "a"], true).status.success());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let latest = loop {
        let state = fs::read_to_string(f.state_path()).unwrap_or_default();
        if state.contains("99.0.0") {
            break state;
        }
        assert!(std::time::Instant::now() < deadline, "no result: {state}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(!latest.contains("100.0.0"), "{latest}");

    let next = f.run(&["query", "a"], true);
    assert!(String::from_utf8_lossy(&next.stderr).contains("drift 99.0.0 is available"));
}

#[test]
fn update_check_reports_on_request() {
    let newer = UpdateFixture::new("update-check-newer", &["v99.0.0"]);
    let output = newer.run(&["update-check"], true);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("drift 99.0.0 is available"));

    let none = UpdateFixture::new("update-check-none", &[]);
    let output = none.run(&["update-check"], true);
    assert!(String::from_utf8_lossy(&output.stdout).contains("no releases"));

    let same = UpdateFixture::new("update-check-same", &["v0.0.1"]);
    let output = same.run(&["update-check"], true);
    assert!(String::from_utf8_lossy(&output.stdout).contains("up to date"));
}

fn transcript_record(kind: &str, content: Value) -> String {
    let now = humantime_now();
    serde_json::json!({"type": kind, "timestamp": now, "message": {"content": content}}).to_string()
}

fn humantime_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // A date far enough ahead to count as recent; the audit only compares these as text.
    format!("9999-01-01T00:00:{:02}Z", seconds % 60)
}

fn transcript_call(id: &str, name: &str, input: Value, output: &str) -> String {
    [
        transcript_record(
            "assistant",
            serde_json::json!([{"type": "tool_use", "id": id, "name": name, "input": input}]),
        ),
        transcript_record(
            "user",
            serde_json::json!([{"type": "tool_result", "tool_use_id": id, "content": output}]),
        ),
    ]
    .join("\n")
}

#[test]
fn audit_reports_totals_without_file_names_or_contents() {
    let fixture = Fixture::new("audit");
    let secret = "SECRET_CONTENT";
    let body = format!("{secret}{}", "x".repeat(3986));
    let lines = [
        transcript_call(
            "1",
            "Read",
            serde_json::json!({"file_path": "src/private_name.ts"}),
            &body,
        ),
        transcript_call(
            "2",
            "Grep",
            serde_json::json!({"pattern": "verifyToken"}),
            &body,
        ),
        transcript_call(
            "3",
            "Bash",
            serde_json::json!({"command": "drift query verifyToken"}),
            "function verifyToken  src/a.ts:1-2",
        ),
        // A resumed session repeats call 1, which must not count twice.
        transcript_call(
            "1",
            "Read",
            serde_json::json!({"file_path": "src/private_name.ts"}),
            &body,
        ),
        "not json".to_string(),
    ]
    .join("\n");
    fixture.write("projects/my-project/session.jsonl", &lines);
    fixture.write("projects/other-project/session.jsonl", &lines);

    let output = stdout(&fixture.drift(&["audit", "--dir", "projects"]));
    assert!(output.contains("3 tool results"), "{output}");
    assert!(output.contains("Reading files"), "{output}");
    assert!(
        output.contains("Drift handled 33% of these calls."),
        "{output}"
    );
    assert!(!output.contains(secret) && !output.contains("private_name"));

    let json = stdout(&fixture.drift(&["audit", "--dir", "projects", "--json"]));
    let parsed: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["finding_code"]["drift_lookups"]["calls"], 1);
    assert_eq!(parsed["finding_code"]["name_searches"]["calls"], 1);
    assert_eq!(parsed["finding_code"]["whole_file_reads"]["calls"], 1);
    assert_eq!(parsed["finding_code"]["whole_file_reads"]["tokens"], 1000);

    fixture.write(
        "projects/third/session.jsonl",
        &transcript_call("9", "Glob", serde_json::json!({"pattern": "*"}), "a"),
    );
    let only = stdout(&fixture.drift(&["audit", "--dir", "projects", "--project", "THIRD"]));
    assert!(only.contains("1 tool result,"), "{only}");
}

#[test]
fn audit_explains_when_there_is_nothing_to_read() {
    let fixture = Fixture::new("audit-empty");
    let missing = fixture.drift(&["audit", "--dir", "nowhere"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--dir"));

    fixture.write("projects/a/session.jsonl", "");
    let empty = fixture.drift(&["audit", "--dir", "projects"]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("No tool results"));
}
