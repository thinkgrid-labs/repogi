//! Transport boundary fixtures. Only Atlas and safe fixture Git plumbing execute.
#![cfg(unix)]
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_repogi");
struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next: u64,
}
impl Server {
    fn start(roots: &[&Path]) -> Self {
        let mut command = Command::new(BINARY);
        command.args(["mcp", "--tools", "atlas_impact"]);
        for root in roots {
            command.arg("--root").arg(root);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take().unwrap();
        let mut server = Self {
            child,
            stdin,
            lines,
            next: 0,
        };
        server.request("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"transport-fixture","version":"1"}}));
        server
    }
    #[allow(clippy::needless_pass_by_value)]
    fn request(&mut self, method: &str, params: Value) -> (Value, usize) {
        self.next += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let line = self.lines.recv_timeout(Duration::from_secs(60)).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.next);
        (response["result"].clone(), line.len())
    }
    #[allow(clippy::needless_pass_by_value)]
    fn call(&mut self, args: Value) -> (String, bool, usize) {
        let (result, wire) = self.request(
            "tools/call",
            json!({"name":"atlas_impact","arguments":args}),
        );
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
            wire,
        )
    }
    fn ok(&mut self, args: Value) -> Value {
        let budget = usize::try_from(
            args.get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(8000),
        )
        .unwrap();
        let (text, error, wire) = self.call(args);
        assert!(!error, "{text}");
        assert!(wire <= budget, "{wire} exceeds {budget}");
        serde_json::from_str(&text).unwrap()
    }
    fn error(&mut self, args: Value, code: &str) {
        let (text, error, _) = self.call(args);
        assert!(
            error && text.starts_with(&format!("error[{code}]")),
            "{text}"
        );
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(root)
        .args([
            "-c",
            "user.name=Atlas",
            "-c",
            "user.email=atlas@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
fn fixture(tests: usize) -> TempDir {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join(".gitignore"), ".code-intel/\n").unwrap();
    fs::write(
        root.path().join("math.ts"),
        "export function f() { return 1; }\n",
    )
    .unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"test":"touch SHOULD_NOT_EXIST"},"private_secret":"NEVER_EXPORT_938204"}"#,
    )
    .unwrap();
    for i in 0..tests {
        fs::write(
            root.path().join(format!("case-{i:03}.test.ts")),
            "import { f } from './math'; test('é', () => f());\n",
        )
        .unwrap();
    }
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["add", "-A"]);
    git(root.path(), &["commit", "-qm", "base"]);
    fs::write(
        root.path().join("math.ts"),
        "export function f() { return 2; }\n",
    )
    .unwrap();
    root
}
fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(BINARY)
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
}
fn canonical(root: &Path) -> Vec<u8> {
    let out = cli(root, &["impact", ".", "--manifest", "--format", "json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}
fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
fn independent(bytes: &[u8], descriptor: &Value) {
    assert_eq!(digest(bytes), descriptor["envelope_digest"]);
    assert_eq!(descriptor["artifact_id"], descriptor["envelope_digest"]);
    assert_eq!(bytes.len() as u64, descriptor["total_bytes"]);
    let mut payload: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(bytes, format!("{payload}\n").as_bytes());
    let id = payload
        .as_object_mut()
        .unwrap()
        .remove("manifest_id")
        .unwrap();
    assert_eq!(id, digest(&serde_json::to_vec(&payload).unwrap()));
    assert_eq!(id, descriptor["manifest_id"]);
    assert_eq!(
        descriptor["snapshots"]["base"],
        payload["snapshots"]["base"]
    );
    assert_eq!(
        descriptor["snapshots"]["head"],
        payload["snapshots"]["head"]
    );
    assert_eq!(payload["schema_version"], 1);
}
fn range(id: &Value, offset: usize, length: usize, budget: usize) -> Value {
    json!({"artifact":{"id":id,"offset":offset,"length":length},"max_bytes":budget})
}
fn reconstruct(server: &mut Server, descriptor: &Value, budget: usize) -> (Vec<u8>, Vec<Value>) {
    let mut bytes = Vec::new();
    let mut chunks = Vec::new();
    loop {
        let part = server.ok(range(
            &descriptor["artifact_id"],
            bytes.len(),
            16000,
            budget,
        ));
        assert_eq!(part["offset"], bytes.len() as u64);
        assert_eq!(part["artifact_id"], descriptor["artifact_id"]);
        assert_eq!(part["envelope_digest"], descriptor["envelope_digest"]);
        assert_eq!(part["total_bytes"], descriptor["total_bytes"]);
        assert_eq!(part["kind"], "verification_manifest_chunk");
        assert_eq!(part["is_manifest"], false);
        let data = STANDARD.decode(part["data"].as_str().unwrap()).unwrap();
        assert!(!data.is_empty() && data.len() <= 16000);
        assert_eq!(part["decoded_bytes"], data.len() as u64);
        bytes.extend(data);
        assert_eq!(part["next_offset"], bytes.len() as u64);
        let final_chunk = bytes.len() as u64 == descriptor["total_bytes"];
        assert_eq!(part["final"], final_chunk);
        chunks.push(part);
        if final_chunk {
            break;
        }
    }
    independent(&bytes, descriptor);
    (bytes, chunks)
}
fn cache(root: &Path) -> PathBuf {
    root.join(".code-intel/impact/artifacts-v1")
}
fn entries(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(cache(root))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect()
}

#[test]
fn small_single_chunk_and_large_many_chunks_equal_cli_and_independent_ids() {
    for (count, budget) in [(1, 32000), (120, 32000), (120, 8000), (8, 2000)] {
        let root = fixture(count);
        let mut server = Server::start(&[root.path()]);
        let descriptor = server.ok(json!({"manifest":true}));
        assert_eq!(descriptor["kind"], "verification_manifest_descriptor");
        assert_eq!(descriptor["is_manifest"], false);
        let (bytes, chunks) = reconstruct(&mut server, &descriptor, budget);
        assert_eq!(bytes, canonical(root.path()));
        if count == 1 {
            assert_eq!(chunks.len(), 1);
        } else {
            assert!(chunks.len() > 1);
        }
        if count == 120 {
            assert!(bytes.len() > 32000);
        }
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains("NEVER_EXPORT_938204") && !text.contains(root.path().to_str().unwrap())
        );
        assert!(!root.path().join("SHOULD_NOT_EXIST").exists());
    }
}

#[test]
fn restart_and_edits_preserve_old_bytes_and_repeated_ranges() {
    let root = fixture(20);
    let before = canonical(root.path());
    let mut server = Server::start(&[root.path()]);
    let descriptor = server.ok(json!({"manifest":true}));
    let first = server.ok(range(&descriptor["artifact_id"], 0, 123, 2000));
    assert_eq!(
        first,
        server.ok(range(&descriptor["artifact_id"], 0, 123, 2000))
    );
    drop(server);
    fs::write(
        root.path().join("math.ts"),
        "export function f() { return 903; }\n",
    )
    .unwrap();
    let mut server = Server::start(&[root.path()]);
    let old = reconstruct(&mut server, &descriptor, 8000).0;
    assert_eq!(old, before);
    let new = server.ok(json!({"manifest":true}));
    assert_ne!(descriptor["artifact_id"], new["artifact_id"]);
    assert_eq!(reconstruct(&mut server, &descriptor, 8000).0, before);
    let (fresh, _) = reconstruct(&mut server, &new, 8000);
    assert_eq!(fresh, canonical(root.path()));
}

#[test]
fn corrupt_missing_repeated_and_reordered_chunks_cannot_validate_as_complete() {
    let root = fixture(20);
    let mut server = Server::start(&[root.path()]);
    let descriptor = server.ok(json!({"manifest":true}));
    let (bytes, parts) = reconstruct(&mut server, &descriptor, 2000);
    assert!(parts.len() > 3);
    let mut corrupted = bytes.clone();
    corrupted[10] ^= 1;
    assert_ne!(digest(&corrupted), descriptor["envelope_digest"]);
    let decoded: Vec<Vec<u8>> = parts
        .iter()
        .map(|p| STANDARD.decode(p["data"].as_str().unwrap()).unwrap())
        .collect();
    let missing = decoded[1..].concat();
    assert_ne!(digest(&missing), descriptor["envelope_digest"]);
    let repeated = [decoded[0].clone(), bytes].concat();
    assert_ne!(digest(&repeated), descriptor["envelope_digest"]);
    let reversed = decoded.into_iter().rev().flatten().collect::<Vec<_>>();
    assert_ne!(digest(&reversed), descriptor["envelope_digest"]);
}

#[test]
fn invalid_arguments_offsets_and_paths_are_errors_without_repository_edits() {
    let root = fixture(1);
    let state = git(root.path(), &["status", "--porcelain=v1", "-z"]);
    let mut server = Server::start(&[root.path()]);
    let d = server.ok(json!({"manifest":true}));
    for args in [
        json!({"manifest":true,"tests":true}),
        json!({"manifest":true,"summary":false}),
        json!({"manifest":true,"detail":false}),
        json!({"manifest":true,"evidence_file":"math.ts"}),
        json!({"manifest":true,"format":"agent"}),
        json!({"manifest":true,"cache_dir":"/tmp"}),
        json!({"manifest":true,"bogus":true}),
        json!({"artifact":null}),
        json!({"artifact":{"id":d["artifact_id"],"offset":0,"length":0}}),
    ] {
        server.error(args, "invalid_arguments");
    }
    for field in [
        "manifest",
        "tests",
        "summary",
        "detail",
        "format",
        "base",
        "head",
        "depth",
        "max_results",
        "test_dirs",
        "evidence_file",
        "cache_dir",
    ] {
        let mut args = range(&d["artifact_id"], 0, 1, 8000);
        args[field] = json!(null);
        server.error(args, "invalid_arguments");
    }
    for offset in [d["total_bytes"].as_u64().unwrap(), u64::MAX] {
        server.error(
            json!({"artifact":{"id":d["artifact_id"],"offset":offset,"length":1}}),
            "invalid_arguments",
        );
    }
    for args in [
        json!({"artifact":{"id":"../../secret","offset":0,"length":1}}),
        json!({"artifact":{"id":d["artifact_id"],"offset":-1,"length":1}}),
        json!({"artifact":{"id":d["artifact_id"],"offset":0.1,"length":1}}),
        json!({"artifact":{"id":d["artifact_id"],"offset":0,"length":16001}}),
        json!({"artifact":{"id":d["artifact_id"],"offset":0,"length":1,"path":"/etc/passwd"}}),
        json!({"artifact":{"id":d["artifact_id"],"length":1}}),
    ] {
        server.error(args, "invalid_arguments");
    }
    server.error(
        json!({"manifest":true,"base":"absent-revision"}),
        "invalid_revision",
    );
    server.error(
        json!({"manifest":true,"base":"--output=/tmp/trap"}),
        "invalid_revision",
    );
    server.error(
        json!({"manifest":true,"test_dirs":["../outside"]}),
        "invalid_path",
    );
    server.error(json!({"manifest":true,"root":"/tmp"}), "root_not_allowed");
    assert_eq!(state, git(root.path(), &["status", "--porcelain=v1", "-z"]));
}

#[test]
fn descriptor_budget_failure_installs_no_artifact() {
    let root = fixture(1);
    let a = "a".repeat(240);
    let b = "b".repeat(240);
    git(root.path(), &["branch", &a]);
    git(root.path(), &["branch", &b]);
    let mut server = Server::start(&[root.path()]);
    server.error(
        json!({"manifest":true,"base":a,"head":b,"max_bytes":2000}),
        "response_too_large",
    );
    assert!(entries(root.path()).is_empty());
    let d = server.ok(json!({"manifest":true,"max_bytes":8000}));
    assert_eq!(reconstruct(&mut server, &d, 2000).0, canonical(root.path()));
}

#[test]
fn cache_corruption_missing_files_zero_length_and_symlink_are_never_served() {
    for mode in [
        "bytes",
        "empty",
        "oversized",
        "missing",
        "symlink",
        "hardlink",
    ] {
        let root = fixture(1);
        let mut server = Server::start(&[root.path()]);
        let d = server.ok(json!({"manifest":true}));
        let path = entries(root.path()).pop().unwrap();
        let external = TempDir::new().unwrap();
        let secret = external.path().join("secret");
        fs::write(&secret, "PRIVATE_CACHE_TRAP").unwrap();
        match mode {
            "bytes" => {
                let mut bytes = fs::read(&path).unwrap();
                bytes[10] ^= 1;
                fs::write(&path, bytes).unwrap();
            }
            "empty" => fs::write(&path, "").unwrap(),
            "oversized" => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(8_000_001)
                .unwrap(),
            "missing" => {
                fs::remove_file(&path).unwrap();
                fs::write(
                    root.path().join("math.ts"),
                    "export function f() { return 999; }\n",
                )
                .unwrap();
            }
            "symlink" => {
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink(&secret, &path).unwrap();
            }
            "hardlink" => {
                fs::remove_file(&path).unwrap();
                fs::hard_link(&secret, &path).unwrap();
            }
            _ => unreachable!(),
        }
        server.error(
            range(&d["artifact_id"], 0, 10, 8000),
            match mode {
                "missing" => "artifact_expired",
                "symlink" | "hardlink" => "state_rejected",
                _ => "artifact_corrupt",
            },
        );
        assert_eq!(fs::read_to_string(secret).unwrap(), "PRIVATE_CACHE_TRAP");
    }
}

#[test]
fn configured_roots_and_copied_cache_are_isolated() {
    let a = fixture(1);
    let b = fixture(1);
    let mut server = Server::start(&[a.path(), b.path()]);
    let d = server.ok(json!({"manifest":true,"root":a.path()}));
    let mut args = range(&d["artifact_id"], 0, 10, 8000);
    args["root"] = json!(b.path());
    server.error(args.clone(), "artifact_expired");
    fs::create_dir_all(cache(b.path())).unwrap();
    for entry in fs::read_dir(cache(a.path())).unwrap() {
        let e = entry.unwrap();
        fs::copy(e.path(), cache(b.path()).join(e.file_name())).unwrap();
    }
    server.error(args, "state_rejected");
}

#[test]
fn symlinked_cache_ancestors_and_lock_are_refused_before_reads_or_writes() {
    for name in [
        ".code-intel",
        ".code-intel/impact",
        ".code-intel/impact/artifacts-v1",
        ".code-intel/impact/artifacts-v1/.lock",
        ".code-intel/impact/artifacts-v1/root",
    ] {
        let root = fixture(1);
        let outside = TempDir::new().unwrap();
        let target = root.path().join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(outside.path(), &target).unwrap();
        let mut server = Server::start(&[root.path()]);
        server.error(json!({"manifest":true}), "state_rejected");
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}

#[test]
fn fifo_eviction_is_bounded_and_duplicate_or_read_does_not_refresh_age() {
    let root = fixture(1);
    let mut server = Server::start(&[root.path()]);
    let first = server.ok(json!({"manifest":true}));
    let second = server.ok(json!({"manifest":true,"depth":1}));
    assert_ne!(first["artifact_id"], second["artifact_id"]);
    assert_eq!(first, server.ok(json!({"manifest":true})));
    server.ok(range(&first["artifact_id"], 0, 1, 8000));
    for count in 1..=31 {
        server.ok(json!({"manifest":true,"max_results":count}));
    }
    assert_eq!(entries(root.path()).len(), 32);
    server.error(range(&first["artifact_id"], 1, 1, 8000), "artifact_expired");
    server.ok(range(&second["artifact_id"], 0, 1, 8000));
}

#[test]
fn concurrent_preparations_are_atomic_or_explicitly_busy_then_reusable() {
    let root = fixture(10);
    let path = root.path().to_path_buf();
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || {
                let mut s = Server::start(&[&path]);
                s.call(json!({"manifest":true}))
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let successful: Vec<_> = results.iter().filter(|(_, error, _)| !*error).collect();
    assert!(!successful.is_empty());
    for (text, error, _) in &results {
        if *error {
            assert!(text.starts_with("error[artifact_cache_busy]"), "{text}");
        }
    }
    let mut s = Server::start(&[root.path()]);
    let d = s.ok(json!({"manifest":true}));
    for (text, _, _) in successful {
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), d);
    }
    assert_eq!(entries(root.path()).len(), 1);
    let expected = canonical(root.path());
    assert_eq!(reconstruct(&mut s, &d, 8000).0, expected);
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let path = path.clone();
            let id = d["artifact_id"].clone();
            thread::spawn(move || Server::start(&[&path]).call(range(&id, 0, 32, 2000)))
        })
        .collect();
    for reader in readers {
        let (text, error, _) = reader.join().unwrap();
        if error {
            assert!(text.starts_with("error[artifact_cache_busy]"), "{text}");
        } else {
            let chunk: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap(),
                expected[..32]
            );
        }
    }
}

#[test]
fn cold_warm_creation_and_existing_modes_preserve_primary_index_and_git() {
    let root = fixture(1);
    assert!(cli(root.path(), &["index", "."]).status.success());
    let db = fs::read(root.path().join(".code-intel/index.db")).unwrap();
    let index = fs::read(root.path().join(".git/index")).unwrap();
    let refs = git(root.path(), &["show-ref"]);
    let status = git(root.path(), &["status", "--porcelain=v1", "-z"]);
    let mut s = Server::start(&[root.path()]);
    let d = s.ok(json!({"manifest":true}));
    let paths = entries(root.path());
    let metadata = fs::metadata(&paths[0]).unwrap();
    assert_eq!(d, s.ok(json!({"manifest":true})));
    assert_eq!(
        metadata.modified().unwrap(),
        fs::metadata(&paths[0]).unwrap().modified().unwrap()
    );
    for (args, flags) in [
        (json!({"format":"json","max_bytes":32000}), vec![]),
        (
            json!({"tests":true,"format":"json","max_bytes":32000}),
            vec!["--tests"],
        ),
        (
            json!({"summary":true,"format":"json","max_bytes":32000}),
            vec!["--summary"],
        ),
        (
            json!({"evidence_file":"case-000.test.ts","format":"json","max_bytes":32000}),
            vec!["--evidence-file", "case-000.test.ts"],
        ),
    ] {
        let actual = s.ok(args);
        let mut command = vec!["impact", ".", "--format", "json", "--max-bytes", "32000"];
        command.extend(flags);
        let output = cli(root.path(), &command);
        assert!(output.status.success());
        let mut expected: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut actual = actual;
        for side in ["base", "head"] {
            for field in ["seconds", "index", "files_reparsed", "seeded"] {
                if let Some(o) = expected[side].as_object_mut() {
                    o.remove(field);
                }
                if let Some(o) = actual[side].as_object_mut() {
                    o.remove(field);
                }
            }
        }
        assert_eq!(actual, expected);
    }
    reconstruct(&mut s, &d, 8000);
    assert_eq!(
        db,
        fs::read(root.path().join(".code-intel/index.db")).unwrap()
    );
    assert_eq!(index, fs::read(root.path().join(".git/index")).unwrap());
    assert_eq!(refs, git(root.path(), &["show-ref"]));
    assert_eq!(
        status,
        git(root.path(), &["status", "--porcelain=v1", "-z"])
    );
}

#[test]
fn ci_generation_and_consumer_validation_fail_cleanly_on_invalid_artifacts() {
    let root = fixture(1);
    git(root.path(), &["add", "math.ts"]);
    git(root.path(), &["commit", "-qm", "head"]);
    let out = cli(
        root.path(),
        &[
            "impact",
            ".",
            "--base",
            "HEAD~1",
            "--head",
            "HEAD",
            "--manifest",
            "--format",
            "json",
        ],
    );
    assert!(out.status.success());
    let path = root.path().join(".code-intel/result.json");
    fs::write(&path, &out.stdout).unwrap();
    let check = Command::new(BINARY)
        .arg("manifest-verify")
        .arg(&path)
        .args(["--expected-envelope-digest", &digest(&out.stdout)])
        .output()
        .unwrap();
    assert!(check.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&check.stdout).unwrap()["valid"],
        true
    );
    for bad in [
        Vec::new(),
        b"{}\n".to_vec(),
        out.stdout[..out.stdout.len() - 1].to_vec(),
        {
            let mut v: Value = serde_json::from_slice(&out.stdout).unwrap();
            v["schema_version"] = json!(2);
            format!("{v}\n").into_bytes()
        },
        {
            let mut v: Value = serde_json::from_slice(&out.stdout).unwrap();
            v["analysis"]["depth"] = json!(4);
            format!("{v}\n").into_bytes()
        },
    ] {
        fs::write(&path, bad).unwrap();
        let r = Command::new(BINARY)
            .arg("manifest-verify")
            .arg(&path)
            .output()
            .unwrap();
        assert!(!r.status.success() && r.stdout.is_empty());
    }
    for args in [
        vec![
            "impact",
            ".",
            "--manifest",
            "--format",
            "json",
            "--max-bytes",
            "2000",
        ],
        vec![
            "impact",
            ".",
            "--manifest",
            "--format",
            "json",
            "--base",
            "missing-object",
        ],
    ] {
        let r = cli(root.path(), &args);
        assert!(!r.status.success() && r.stdout.is_empty());
    }
}

#[test]
fn byte_offsets_may_split_utf8_and_non_git_roots_fail_explicitly() {
    let root = fixture(1);
    fs::rename(
        root.path().join("case-000.test.ts"),
        root.path().join("case-é.test.ts"),
    )
    .unwrap();
    let mut server = Server::start(&[root.path()]);
    let descriptor = server.ok(json!({"manifest":true}));
    let (bytes, _) = reconstruct(&mut server, &descriptor, 8000);
    let position = bytes.iter().position(|b| *b == 0xc3).unwrap();
    for offset in [position, position + 1] {
        let chunk = server.ok(range(&descriptor["artifact_id"], offset, 1, 2000));
        assert_eq!(
            STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap(),
            &bytes[offset..=offset]
        );
    }
    let nongit = TempDir::new().unwrap();
    let mut server = Server::start(&[nongit.path()]);
    server.error(json!({"manifest":true}), "not_a_git_repository");
}
