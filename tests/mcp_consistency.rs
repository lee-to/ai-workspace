use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

fn command(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-workspace"));
    command.env("AI_WORKSPACE_DB", db);
    for key in [
        "AI_WORKSPACE_SCOPE",
        "AI_WORKSPACE_SCOPE_GROUP",
        "AI_WORKSPACE_SCOPE_PROJECT",
        "AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS",
        "AI_WORKSPACE_ALLOW_PROJECT_FILE_WRITE",
    ] {
        command.env_remove(key);
    }
    command
}

fn cli(db: &Path, project: &Path, args: &[&str]) {
    let output = command(db)
        .current_dir(project)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Client {
    child: Child,
    input: ChildStdin,
    messages: Receiver<Value>,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Client {
    fn start(db: &Path, scope: &[&str]) -> Self {
        let mut child = command(db)
            .arg("serve")
            .args(scope)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if sender.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            messages,
        }
    }

    // Each response is a synchronization barrier before the fixture mutation.
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0", "id":1,
            "method":"tools/call", "params":{"name":name,"arguments":arguments}})
        )
        .unwrap();
        self.messages.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    fn search(&mut self, query: &str) -> Value {
        let response = self.call("workspace_search_fulltext", json!({"query":query}));
        assert_ne!(response["result"]["isError"], true, "{response}");
        serde_json::from_str(text(&response)).unwrap()
    }
}

fn text(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}

fn seed(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let db = root.join("workspace.db");
    let project = root.join("p");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::write(project.join("docs/a.md"), "old_marker\r\nПривет").unwrap();
    cli(&db, &project, &["init", "--slug", "p", "--group", "team"]);
    cli(&db, &project, &["share", "docs"]);
    (db, project)
}

fn read_args(hit: &Value) -> Value {
    json!({"project_id":hit["project_id"],"rel_path":hit["path"],
        "expected_content_hash":hit["content_hash"]})
}

#[test]
fn search_read_detects_same_size_same_mtime_edit_and_preserves_plain_reads() {
    let root = tempfile::tempdir().unwrap();
    let (db, project) = seed(root.path());
    let mut client = Client::start(&db, &[]);
    let hits = client.search("old_marker");
    let hit = &hits[0];
    assert_eq!(
        hit["content_hash"],
        format!("{:x}", Sha256::digest("old_marker\r\nПривет".as_bytes()))
    );
    let args = read_args(hit);
    let read = client.call("workspace_read", args.clone());
    assert_eq!(text(&read), "old_marker\r\nПривет");

    let path = project.join("docs/a.md");
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "new_marker\r\nПривет").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let stale = client.call("workspace_read", args);
    assert_eq!(stale["result"]["isError"], true);
    assert!(text(&stale).contains("content hash mismatch"));
    assert!(!text(&stale).contains("new_marker"));
    let plain = client.call(
        "workspace_read",
        json!({"project_id":hit["project_id"],"rel_path":hit["path"]}),
    );
    assert_eq!(text(&plain), "new_marker\r\nПривет");
    cli(&db, &project, &["reindex"]);
    let fresh = client.search("new_marker");
    assert_ne!(fresh[0]["content_hash"], hit["content_hash"]);
    assert_eq!(
        text(&client.call("workspace_read", read_args(&fresh[0]))),
        "new_marker\r\nПривет"
    );
}

#[test]
fn retained_hit_cannot_read_deleted_file_or_revoked_directory_share() {
    let root = tempfile::tempdir().unwrap();
    let (db, project) = seed(root.path());
    let mut client = Client::start(&db, &[]);
    let hit = client.search("old_marker")[0].clone();
    std::fs::remove_file(project.join("docs/a.md")).unwrap();
    assert_eq!(
        client.call("workspace_read", read_args(&hit))["result"]["isError"],
        true
    );
    std::fs::write(project.join("docs/a.md"), "old_marker\r\nПривет").unwrap();
    cli(
        &db,
        &project,
        &[
            "rm",
            hit["shared_item_id"].as_i64().unwrap().to_string().as_str(),
        ],
    );
    let denied = client.call("workspace_read", read_args(&hit));
    assert_eq!(denied["result"]["isError"], true);
    assert!(text(&denied).contains("shared"));
    let item_read = client.call("workspace_read", json!({"item_id":hit["shared_item_id"]}));
    assert_eq!(item_read["result"]["isError"], true);
    cli(&db, &project, &["reindex"]);
    assert!(client.search("old_marker").as_array().unwrap().is_empty());
    assert_eq!(
        client.call("workspace_read", read_args(&hit))["result"]["isError"],
        true
    );
}

#[test]
fn group_membership_revocation_applies_in_open_mcp_session() {
    let root = tempfile::tempdir().unwrap();
    let (db, project) = seed(root.path());
    let other = root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    cli(&db, &other, &["init", "--slug", "other", "--group", "team"]);
    let mut client = Client::start(&db, &["--group", "team"]);
    let hit = client.search("old_marker")[0].clone();
    cli(&db, &project, &["leave", "team"]);
    let denied = client.call("workspace_read", read_args(&hit));
    assert_eq!(denied["result"]["isError"], true);
    assert!(text(&denied).contains("scope"));
    let item_read = client.call("workspace_read", json!({"item_id":hit["shared_item_id"]}));
    assert_eq!(item_read["result"]["isError"], true);
    assert!(client.search("old_marker").as_array().unwrap().is_empty());
    cli(&db, &project, &["reindex"]);
    assert_eq!(
        client.call("workspace_read", read_args(&hit))["result"]["isError"],
        true
    );
}

#[test]
fn project_scope_loses_group_notes_after_leaving_group() {
    let root = tempfile::tempdir().unwrap();
    let (db, project) = seed(root.path());
    let other = root.path().join("other");
    std::fs::create_dir(&other).unwrap();
    cli(&db, &other, &["init", "--slug", "other", "--group", "team"]);
    cli(
        &db,
        &other,
        &[
            "note",
            "group_note_marker",
            "--scope",
            "group",
            "--group",
            "team",
        ],
    );
    let mut client = Client::start(&db, &["--project", "p"]);
    let response = client.call("workspace_search", json!({"query":"group_note_marker"}));
    let notes: Value = serde_json::from_str(text(&response)).unwrap();
    let note_id = notes[0]["id"].clone();
    assert!(note_id.is_number(), "{notes}");
    assert_eq!(
        text(&client.call("workspace_read", json!({"item_id":note_id}))),
        "group_note_marker"
    );
    cli(&db, &project, &["leave", "team"]);
    let denied = client.call("workspace_read", json!({"item_id":note_id}));
    assert_eq!(denied["result"]["isError"], true);
    assert!(text(&denied).contains("scope"));
    // Project selection stays valid; only the revoked group membership changes.
    assert_eq!(client.search("old_marker").as_array().unwrap().len(), 1);
}

#[test]
fn expected_hash_validation_and_directory_rejection() {
    let root = tempfile::tempdir().unwrap();
    let (db, _) = seed(root.path());
    let mut client = Client::start(&db, &[]);
    let hit = client.search("old_marker")[0].clone();
    for invalid in [json!(null), json!(3), json!("bad"), json!("A".repeat(64))] {
        let mut args = read_args(&hit);
        args["expected_content_hash"] = invalid;
        assert_eq!(client.call("workspace_read", args)["error"]["code"], -32602);
    }
    assert_eq!(
        client.call(
            "workspace_read",
            json!({"item_id":hit["shared_item_id"],
        "expected_content_hash":hit["content_hash"]})
        )["error"]["code"],
        -32602
    );
    let mut args = read_args(&hit);
    args["rel_path"] = json!("docs");
    let directory = client.call("workspace_read", args);
    assert_eq!(directory["result"]["isError"], true);
    assert!(text(&directory).contains("only supported for files"));
}
