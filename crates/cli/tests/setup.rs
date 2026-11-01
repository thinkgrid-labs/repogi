//! `repogi init`, `doctor` and `agent add`: idempotent, read-only where
//! promised, never overwriting configuration or touching memories.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");

fn repogi(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let output = repogi(cwd, args);
    assert!(
        output.status.success(),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Repogi Test",
            "-c",
            "user.email=repogi@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn repository() -> (TempDir, PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src/deep")).unwrap();
    fs::write(root.join("src/app.js"), "export function app() {}\n").unwrap();
    fs::write(
        root.join("src/view.tsx"),
        "export const View = () => <div />;\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# Project\n").unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "init"]);
    (temp, root)
}

/// Every file under `root` except `.git`, with its bytes.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn doctor(cwd: &Path, args: &[&str]) -> (Value, bool) {
    let mut all = vec!["doctor", "--format", "json"];
    all.extend_from_slice(args);
    let output = repogi(cwd, &all);
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        output.status.success(),
    )
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {report}"))
}

#[test]
fn init_is_idempotent_from_a_subdirectory_and_changes_no_tracked_file() {
    let (_temp, root) = repository();
    let before = snapshot(&root);
    let dry: Value = serde_json::from_str(&ok(
        &root.join("src/deep"),
        &["init", "--dry-run", "--format", "json"],
    ))
    .unwrap();
    assert_eq!(dry["root"], root.to_str().unwrap());
    assert_eq!(dry["root_source"], "git");
    assert_eq!(dry["sources"]["javascript"], 1);
    assert_eq!(dry["sources"]["tsx"], 1);
    assert_eq!(dry["documents"], 1);
    assert_eq!(snapshot(&root), before, "--dry-run writes nothing");

    let first = ok(&root.join("src/deep"), &["init"]);
    assert!(first.contains("wrote .code-intel/.gitignore"), "{first}");
    assert_eq!(
        git(&root, &["status", "--porcelain"]),
        "",
        "no tracked or visible change"
    );
    let second = ok(&root, &["init"]);
    assert!(second.contains("nothing to change"), "{second}");
    assert!(
        !root.join(".code-intel/index.db").exists(),
        "init indexes only with --index"
    );

    let indexed: Value =
        serde_json::from_str(&ok(&root, &["init", "--index", "--format", "json"])).unwrap();
    assert_eq!(indexed["index"]["present"], true);
    assert_eq!(indexed["index"]["languages"]["javascript"], 1);
    assert_eq!(git(&root, &["status", "--porcelain"]), "");
    assert!(
        !root.join(".repogi").exists(),
        "init never creates the memory store"
    );
}

#[test]
fn init_keeps_an_existing_ignore_file_and_refuses_symlinked_state() {
    let (_temp, root) = repository();
    fs::create_dir(root.join(".code-intel")).unwrap();
    fs::write(root.join(".code-intel/.gitignore"), "index.db\n").unwrap();
    ok(&root, &["init"]);
    assert_eq!(
        fs::read_to_string(root.join(".code-intel/.gitignore")).unwrap(),
        "index.db\n"
    );

    let (_other, linked) = repository();
    let outside = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), linked.join(".code-intel")).unwrap();
        let output = repogi(&linked, &["init"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("symlinked index state"));
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[test]
fn doctor_is_read_only_and_reports_actionable_states() {
    let (_temp, root) = repository();
    let before = snapshot(&root);
    let (report, success) = doctor(&root.join("src"), &[]);
    assert!(success, "warnings do not fail: {report}");
    assert_eq!(report["status"], "warning");
    assert_eq!(check(&report, "index")["status"], "warning");
    assert!(
        check(&report, "index")["fix"]
            .as_str()
            .unwrap()
            .contains("repogi index")
    );
    assert_eq!(check(&report, "ignore")["status"], "warning");
    assert_eq!(snapshot(&root), before, "doctor writes nothing");

    ok(&root, &["init", "--index"]);
    let indexed = snapshot(&root);
    let (report, success) = doctor(&root, &["--freshness"]);
    assert!(success);
    assert_eq!(report["status"], "ok", "{report}");
    assert_eq!(check(&report, "coverage")["status"], "ok");
    assert_eq!(check(&report, "freshness")["status"], "ok");
    assert_eq!(
        snapshot(&root),
        indexed,
        "doctor --freshness writes nothing"
    );

    fs::write(root.join("src/new.mjs"), "export const n = 1;\n").unwrap();
    let (report, _) = doctor(&root, &[]);
    assert_eq!(check(&report, "freshness")["status"], "skipped");
    let (report, _) = doctor(&root, &["--freshness"]);
    assert_eq!(check(&report, "freshness")["status"], "warning");
}

#[test]
fn doctor_flags_an_index_from_a_javascript_unaware_build() {
    let (_temp, root) = repository();
    ok(&root, &["index", "."]);
    // What an earlier build left: the same index without JavaScript rows.
    let connection = rusqlite::Connection::open(root.join(".code-intel/index.db")).unwrap();
    connection
        .execute("DELETE FROM files WHERE language='javascript'", [])
        .unwrap();
    drop(connection);
    let (report, _) = doctor(&root, &[]);
    let coverage = check(&report, "coverage");
    assert_eq!(coverage["status"], "warning", "{report}");
    assert!(coverage["summary"].as_str().unwrap().contains("javascript"));
}

#[test]
fn doctor_reports_copied_indexes_and_unreadable_memory_without_suggesting_deletion() {
    let (_temp, root) = repository();
    ok(&root, &["index", "."]);
    let copy = TempDir::new().unwrap();
    let copy = copy.path().canonicalize().unwrap();
    for entry in ["src", ".code-intel"] {
        let status = Command::new("cp")
            .args(["-R"])
            .arg(root.join(entry))
            .arg(&copy)
            .status()
            .unwrap();
        assert!(status.success());
    }
    fs::create_dir(copy.join(".repogi")).unwrap();
    fs::write(copy.join(".repogi/.gitignore"), "*\n").unwrap();
    fs::write(copy.join(".repogi/memory.db"), "not a database").unwrap();
    let (report, success) = doctor(&copy, &[]);
    assert!(!success, "errors fail the command");
    assert_eq!(report["status"], "error");
    let index = check(&report, "index");
    assert_eq!(index["status"], "error");
    assert!(index["summary"].as_str().unwrap().contains("copied"));
    let memory = check(&report, "memory");
    assert_eq!(memory["status"], "error", "{report}");
    let advice = report.to_string();
    assert!(!advice.contains("delete .repogi") && !advice.contains("rm -rf"));
    assert_eq!(
        fs::read(copy.join(".repogi/memory.db")).unwrap(),
        b"not a database"
    );
}

#[test]
fn agent_add_prints_by_default_and_never_overwrites() {
    let (_temp, root) = repository();
    let root_text = root.to_str().unwrap();
    let printed = ok(&root, &["agent", "add", "claude", "--root", root_text]);
    assert!(printed.contains("Nothing was written") && printed.contains("\"--root\""));
    assert!(
        printed.contains("claude mcp add --scope local repogi --"),
        "{printed}"
    );
    assert!(!root.join(".mcp.json").exists());

    ok(
        &root,
        &["agent", "add", "claude", "--root", root_text, "--write"],
    );
    let written: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    let server = &written["mcpServers"]["repogi"];
    assert_eq!(server["args"][2], root_text);
    assert!(Path::new(server["command"].as_str().unwrap()).is_absolute());
    assert_eq!(
        server["args"][6],
        "atlas_context,atlas_references,atlas_dependencies"
    );
    let again = ok(
        &root,
        &["agent", "add", "claude", "--root", root_text, "--write"],
    );
    assert!(again.contains("nothing to do"));
    let (report, _) = doctor(&root, &[]);
    assert_eq!(check(&report, "agent_claude")["status"], "ok");

    let bytes = fs::read(root.join(".mcp.json")).unwrap();
    let different = repogi(
        &root,
        &[
            "agent",
            "add",
            "claude",
            "--root",
            root_text,
            "--tools",
            "atlas_context",
            "--write",
        ],
    );
    assert!(!different.status.success());
    assert!(String::from_utf8_lossy(&different.stderr).contains("different `repogi` entry"));
    let other = repogi(
        &root,
        &[
            "agent",
            "add",
            "claude",
            "--root",
            root_text,
            "--name",
            "repogi-two",
            "--write",
        ],
    );
    assert!(
        !other.status.success(),
        "an existing .mcp.json is never rewritten"
    );
    assert_eq!(fs::read(root.join(".mcp.json")).unwrap(), bytes);

    for args in [
        vec![
            "agent",
            "add",
            "claude",
            "--root",
            root_text,
            "--tools",
            "atlas_shell",
        ],
        vec![
            "agent", "add", "claude", "--root", root_text, "--name", "bad name",
        ],
        vec!["agent", "add", "claude"],
    ] {
        assert!(!repogi(&root, &args).status.success(), "{args:?}");
    }
}

#[test]
fn codex_project_configuration_is_printed_then_created_once() {
    let (_temp, root) = repository();
    let root_text = root.to_str().unwrap();
    let printed = ok(&root, &["agent", "add", "codex", "--root", root_text]);
    assert!(printed.contains("[mcp_servers.repogi]") && printed.contains("Nothing was written"));
    assert!(printed.contains("trust the project") && printed.contains("codex -c"));
    assert!(!root.join(".codex").exists());
    ok(
        &root,
        &["agent", "add", "codex", "--root", root_text, "--write"],
    );
    let written = fs::read_to_string(root.join(".codex/config.toml")).unwrap();
    assert!(
        written.starts_with("[mcp_servers.repogi]\ncommand = \"/"),
        "{written}"
    );
    assert!(written.contains(&format!("\"--root\", \"{root_text}\"")));
    assert!(
        ok(
            &root,
            &["agent", "add", "codex", "--root", root_text, "--write"]
        )
        .contains("nothing to do")
    );
    let (report, _) = doctor(&root, &[]);
    assert_eq!(check(&report, "agent_codex")["status"], "ok", "{report}");
    let different = repogi(
        &root,
        &[
            "agent",
            "add",
            "codex",
            "--root",
            root_text,
            "--tools",
            "atlas_context",
            "--write",
        ],
    );
    assert!(!different.status.success());
    assert_eq!(
        fs::read_to_string(root.join(".codex/config.toml")).unwrap(),
        written
    );
    let (_other, other) = repository();
    fs::create_dir(other.join(".codex")).unwrap();
    fs::write(
        other.join(".codex/config.toml"),
        written.replace(root_text, "/elsewhere"),
    )
    .unwrap();
    let (report, _) = doctor(&other, &[]);
    assert_eq!(
        check(&report, "agent_codex")["status"],
        "warning",
        "{report}"
    );
}

#[test]
fn doctor_rejects_agent_entries_for_another_repository() {
    let (_temp, root) = repository();
    let (_other, other) = repository();
    let config = serde_json::json!({"mcpServers": {"repogi": {
        "type": "stdio",
        "command": BINARY,
        "args": ["mcp", "--root", other.to_str().unwrap()],
    }}});
    fs::write(root.join(".mcp.json"), config.to_string()).unwrap();
    let (report, success) = doctor(&root, &[]);
    assert!(!success);
    let agent = check(&report, "agent_claude");
    assert_eq!(agent["status"], "error");
    assert!(
        agent["summary"]
            .as_str()
            .unwrap()
            .contains("not this repository")
    );
}
