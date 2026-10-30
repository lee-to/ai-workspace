use rusqlite::Connection;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

fn command(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-workspace"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AI_WORKSPACE_") {
            command.env_remove(key);
        }
    }
    command.env("AI_WORKSPACE_DB", db).env("NO_COLOR", "1");
    command
}

fn cli(db: &Path, cwd: &Path, args: &[&str]) -> Output {
    command(db).current_dir(cwd).args(args).output().unwrap()
}

fn ok(db: &Path, cwd: &Path, args: &[&str]) -> String {
    let output = cli(db, cwd, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn context(db: &Path, cwd: &Path) -> Value {
    serde_json::from_str(&ok(db, cwd, &["context"])).unwrap()
}

fn seed(root: &Path) -> (PathBuf, PathBuf) {
    let db = root.join("workspace.db");
    let project = root.join("a");
    std::fs::create_dir_all(project.join("docs")).unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(project.join("docs/a.md"), "alpha_marker\n").unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn old_symbol() -> i32 { 1 }\n",
    )
    .unwrap();
    ok(
        &db,
        &project,
        &["init", "--slug", "sample", "--group", "team"],
    );
    ok(&db, &project, &["share", "docs"]);
    ok(&db, &project, &["share", "src"]);
    (db, project)
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
    fn start(db: &Path, cwd: &Path, scope: &[&str]) -> Self {
        let mut child = command(db)
            .current_dir(cwd)
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
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}})
        )
        .unwrap();
        self.input.flush().unwrap();
        self.messages.recv_timeout(Duration::from_secs(15)).unwrap()
    }
}
fn text(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
fn value(response: Value) -> Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(text(&response)).unwrap()
}
fn denied(response: &Value, message: &str) {
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert!(text(response).contains(message), "{response}");
}
fn git(cwd: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(cwd)
        .args([
            "-c",
            "safe.directory=*",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn two_worktrees_same_branch_require_explicit_checkout_and_keep_dirty_content_separate() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    git(&a, &["init", "--initial-branch=fixture"]);
    git(&a, &["add", "src/lib.rs", "docs/a.md"]);
    git(&a, &["commit", "-m", "fixture"]);
    let b = root.path().join("b");
    git(
        &a,
        &["worktree", "add", "--force", b.to_str().unwrap(), "fixture"],
    );
    std::fs::write(b.join("docs/a.md"), "bravo_marker\n").unwrap();
    let attached: Value =
        serde_json::from_str(&ok(&db, &b, &["worktree", "add", "--project", "sample"])).unwrap();
    let aid = context(&db, &a)["project_id"].as_i64().unwrap();
    let bid = attached["project_id"].as_i64().unwrap();
    let conn = Connection::open(&db).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM project_groups WHERE project_id = ?1",
            [bid],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM shared_items WHERE project_id = ?1",
            [bid],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    ok(&db, &b, &["share", "docs"]);
    ok(&db, &b, &["share", "src"]);
    let ac = context(&db, &a);
    let bc = context(&db, &b);
    assert_eq!(ac["logical_project_id"], bc["logical_project_id"]);
    assert_ne!(ac["binding_id"], bc["binding_id"]);
    assert_eq!(ac["revision"], bc["revision"]);
    assert!(ac["revision"].is_string());
    assert_ne!(ac["source_digest"], bc["source_digest"]);
    assert_ne!(ac["snapshot_id"], bc["snapshot_id"]);
    assert!(!ac.to_string().contains(a.to_str().unwrap()));
    let ambiguous = cli(&db, &a, &["context", "--project", "sample"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("Multiple checkouts"));
    let mut client = Client::start(&db, &a, &[]);
    let denied_selection = client.call("project_context", json!({"project":"sample"}));
    denied(&denied_selection, "Multiple checkouts");
    for (id, expected) in [(aid, "alpha_marker"), (bid, "bravo_marker")] {
        let response = client.call(
            "workspace_read",
            json!({"project_id":id,"rel_path":"docs/a.md"}),
        );
        assert!(text(&response).contains(expected));
        assert_eq!(
            response["result"]["_meta"]["ai_workspace"]["project_id"],
            id
        );
    }
    denied(
        &client.call(
            "workspace_read",
            json!({"project_id":bid,"rel_path":"docs/a.md",
        "expected_snapshot_id":ac["snapshot_id"]}),
        ),
        "Stale context",
    );
    let hits = value(client.call("workspace_search_fulltext", json!({"query":"bravo_marker"})));
    assert_eq!(hits[0]["context"]["binding_id"], bc["binding_id"]);
    assert_eq!(hits[0]["stale"], false);
    let path = a.join("docs/a.md");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "other_marker\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    let changed = context(&db, &a);
    assert_eq!(ac["revision"], changed["revision"]);
    assert_ne!(ac["source_digest"], changed["source_digest"]);
    denied(
        &client.call(
            "workspace_read",
            json!({"project_id":aid,"rel_path":"docs/a.md",
        "expected_snapshot_id":ac["snapshot_id"]}),
        ),
        "Stale context",
    );
}

#[test]
fn codegraph_rejects_dirty_and_new_sources_until_explicit_sync() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    ok(&db, &a, &["codegraph", "reindex"]);
    let id = context(&db, &a)["project_id"].clone();
    let mut client = Client::start(&db, &a, &[]);
    let hits = value(client.call(
        "codegraph_search",
        json!({"project_id":id,"query":"old_symbol"}),
    ));
    let old = hits[0]["node_id"].clone();
    let path = a.join("src/lib.rs");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::fs::write(&path, "pub fn new_symbol() -> i32 { 2 }\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(mtime))
        .unwrap();
    denied(
        &client.call(
            "codegraph_node",
            json!({"project_id":id,"node_id":old,"include_source":true}),
        ),
        "Stale CodeGraph index",
    );
    assert!(
        !cli(&db, &a, &["codegraph", "search", "old_symbol"])
            .status
            .success()
    );
    ok(&db, &a, &["codegraph", "sync"]);
    let hits = value(client.call(
        "codegraph_search",
        json!({"project_id":id,"query":"new_symbol"}),
    ));
    let response = client.call(
        "codegraph_node",
        json!({"project_id":id,"node_id":hits[0]["node_id"],"include_source":true}),
    );
    let node = value(response);
    assert_eq!(node["name"], "new_symbol");
    assert!(node["source"].as_str().unwrap().contains("new_symbol"));
    std::fs::write(a.join("src/added.rs"), "pub fn added() {}\n").unwrap();
    denied(
        &client.call(
            "codegraph_search",
            json!({"project_id":id,"query":"new_symbol"}),
        ),
        "Stale CodeGraph index",
    );
    ok(&db, &a, &["codegraph", "sync"]);
    assert_eq!(
        value(client.call("codegraph_search", json!({"project_id":id,"query":"added"})))[0]["name"],
        "added"
    );
}

#[test]
fn cached_snapshot_cannot_bypass_revoked_shares_or_group_scope() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    let id = context(&db, &a)["project_id"].clone();
    ok(&db, &a, &["codegraph", "reindex"]);
    let mut client = Client::start(&db, &a, &["--group", "team"]);
    let hits = value(client.call("workspace_search_fulltext", json!({"query":"alpha_marker"})));
    let hit = &hits[0];
    let args = json!({"project_id":id,"rel_path":"docs/a.md","expected_snapshot_id":hit["context"]["snapshot_id"],
        "expected_content_hash":hit["content_hash"]});
    assert_eq!(
        text(&client.call("workspace_read", args.clone())),
        "alpha_marker\n"
    );
    ok(&db, &a, &["rm", &hit["shared_item_id"].to_string()]);
    denied(&client.call("workspace_read", args.clone()), "shared");
    denied(
        &client.call("workspace_read", json!({"item_id":hit["shared_item_id"]})),
        "not found",
    );
    assert!(ok(&db, &a, &["search", "alpha_marker"]).contains("No matches"));
    assert!(
        value(client.call("workspace_search_fulltext", json!({"query":"alpha_marker"})))
            .as_array()
            .unwrap()
            .is_empty()
    );
    let symbols = value(client.call(
        "codegraph_search",
        json!({"project_id":id,"query":"old_symbol"}),
    ));
    let src_id: i64 = Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT id FROM shared_items WHERE project_id = ?1 AND path = 'src'",
            [id.as_i64().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    ok(&db, &a, &["rm", &src_id.to_string()]);
    denied(
        &client.call(
            "codegraph_node",
            json!({"project_id":id,"node_id":symbols[0]["node_id"],"include_source":true}),
        ),
        "shared",
    );
    assert!(ok(&db, &a, &["codegraph", "search", "old_symbol"]).contains("No CodeGraph matches"));
    ok(&db, &a, &["leave", "team"]);
    denied(&client.call("workspace_read", args), "scope");
}

#[test]
fn relocation_and_reused_path_never_rebind_implicitly() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    let original = context(&db, &a);
    let id = original["project_id"].as_i64().unwrap().to_string();
    let mut client = Client::start(&db, root.path(), &[]);
    let moved = root.path().join("moved");
    std::fs::rename(&a, &moved).unwrap();
    std::fs::create_dir_all(a.join("docs")).unwrap();
    std::fs::write(a.join("docs/a.md"), "replacement_marker").unwrap();
    denied(
        &client.call(
            "workspace_read",
            json!({"project_id":original["project_id"],"rel_path":"docs/a.md"}),
        ),
        "Checkout identity",
    );
    assert!(ok(&db, &a, &["search", "alpha_marker"]).contains("No matches"));
    let relocated: Value = serde_json::from_str(&ok(
        &db,
        &moved,
        &["worktree", "relocate", "--project", &id],
    ))
    .unwrap();
    assert_eq!(relocated["binding_id"], original["binding_id"]);
    assert_ne!(relocated["snapshot_id"], original["snapshot_id"]);
    assert_eq!(
        text(&client.call(
            "workspace_read",
            json!({"project_id":original["project_id"],"rel_path":"docs/a.md"})
        )),
        "alpha_marker\n"
    );
    std::fs::write(
        moved.join(".ai-workspace-id"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    denied(
        &client.call(
            "workspace_read",
            json!({"project_id":original["project_id"],"rel_path":"docs/a.md"}),
        ),
        "Checkout identity mismatch",
    );
}

#[test]
fn legacy_single_checkout_migrates_without_paths_in_metadata() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    let old = context(&db, &a);
    Connection::open(&db)
        .unwrap()
        .execute_batch("DROP TABLE project_bindings; PRAGMA user_version=7;")
        .unwrap();
    let new = context(&db, &a);
    assert_eq!(old["project_id"], new["project_id"]);
    assert!(!new.to_string().contains("path"));
    assert!(!new.to_string().contains(root.path().to_str().unwrap()));
    assert!(
        cli(&db, &a, &["context", "--project", "sample"])
            .status
            .success()
    );
    let before = context(&db, &a);
    std::fs::write(a.join(".env"), "SECRET=not-shared").unwrap();
    assert_eq!(before["snapshot_id"], context(&db, &a)["snapshot_id"]);
}

#[test]
fn large_shared_scope_preserves_plain_reads_without_claiming_a_complete_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    let original = context(&db, &a);
    std::fs::File::create(a.join("docs/large.bin"))
        .unwrap()
        .set_len(65 * 1024 * 1024)
        .unwrap();
    let mut client = Client::start(&db, &a, &[]);
    let read = client.call(
        "workspace_read",
        json!({"project_id":original["project_id"],"rel_path":"docs/a.md"}),
    );
    assert_eq!(text(&read), "alpha_marker\n");
    assert_eq!(read["result"]["_meta"]["ai_workspace"]["complete"], false);
    assert!(
        read["result"]["_meta"]["ai_workspace"]
            .get("snapshot_id")
            .is_none()
    );
    let hits = value(client.call("workspace_search_fulltext", json!({"query":"alpha_marker"})));
    assert_eq!(hits[0]["context"]["complete"], false);
    assert!(hits[0]["context"].get("snapshot_id").is_none());
    assert_eq!(hits[0]["stale"], false);
    assert!(ok(&db, &a, &["search", "alpha_marker"]).contains("complete=false"));
    denied(
        &client.call(
            "workspace_read",
            json!({"project_id":original["project_id"],"rel_path":"docs/a.md",
        "expected_snapshot_id":original["snapshot_id"]}),
        ),
        "snapshot limit",
    );
    denied(
        &client.call(
            "project_context",
            json!({"project_id":original["project_id"]}),
        ),
        "snapshot limit",
    );
}

#[test]
fn unregistered_nested_worktree_cannot_inherit_its_parents_context() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    git(&a, &["init", "--initial-branch=fixture"]);
    git(&a, &["add", "src/lib.rs", "docs/a.md"]);
    git(&a, &["commit", "-m", "fixture"]);
    let b = a.join("checkouts/b");
    git(
        &a,
        &["worktree", "add", "--detach", b.to_str().unwrap(), "HEAD"],
    );
    let response = cli(&db, &b, &["context"]);
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stderr).contains("unregistered Git checkout"));
    let before = context(&db, &a);
    let attached: Value =
        serde_json::from_str(&ok(&db, &b, &["worktree", "add", "--project", "sample"])).unwrap();
    assert_eq!(before["logical_project_id"], attached["logical_project_id"]);
    assert_ne!(before["binding_id"], context(&db, &b)["binding_id"]);
}

#[test]
fn unavailable_checkout_does_not_block_other_projects_or_rewrite_its_index() {
    let root = tempfile::tempdir().unwrap();
    let (db, a) = seed(root.path());
    std::fs::write(a.join("extra.md"), "unindexed_a_marker").unwrap();
    ok(&db, &a, &["share", "extra.md"]);
    let a_id = context(&db, &a)["project_id"].as_i64().unwrap();
    let b = root.path().join("b");
    std::fs::create_dir_all(b.join("docs")).unwrap();
    std::fs::write(b.join("docs/b.md"), "alpha_marker beta_marker").unwrap();
    ok(&db, &b, &["init", "--slug", "b"]);
    ok(&db, &b, &["share", "docs"]);
    let b_id = context(&db, &b)["project_id"].as_i64().unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute("DELETE FROM indexed_files WHERE shared_item_id IN (SELECT id FROM shared_items WHERE project_id = ?1 AND path = 'extra.md')", [a_id]).unwrap();
    let a_index = || -> Vec<(i64, String, String)> {
        conn.prepare("SELECT i.id, i.abs_path, f.content FROM indexed_files i JOIN files_fts f ON f.rowid = i.id JOIN shared_items s ON s.id = i.shared_item_id WHERE s.project_id = ?1 ORDER BY i.id")
            .unwrap().query_map([a_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    };
    let original_index = a_index();
    assert_eq!(original_index.len(), 1);
    std::fs::write(a.join("docs/a.md"), "changed_a_marker").unwrap();
    let mut scoped = Client::start(&db, &b, &["--project", "b"]);
    let mut global = Client::start(&db, &b, &[]);
    let search = |client: &mut Client| {
        let hits = value(client.call("workspace_search_fulltext", json!({"query":"alpha_marker"})));
        assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
        assert_eq!(hits[0]["project_id"], b_id);
        assert!(hits[0]["snippet"].as_str().unwrap().contains("beta_marker"));
    };
    // Even a healthy out-of-scope A must not be refreshed or newly indexed.
    search(&mut scoped);
    assert_eq!(a_index(), original_index);
    let marker = std::fs::read_to_string(a.join(".ai-workspace-id")).unwrap();
    let moved = root.path().join("moved-a");
    for state in ["moved", "missing-marker", "mismatched-marker"] {
        match state {
            "moved" => std::fs::rename(&a, &moved).unwrap(),
            "missing-marker" => {
                std::fs::rename(a.join(".ai-workspace-id"), a.join(".saved-id")).unwrap()
            }
            _ => std::fs::write(a.join(".ai-workspace-id"), uuid::Uuid::new_v4().to_string())
                .unwrap(),
        }
        search(&mut scoped);
        search(&mut global);
        assert!(ok(&db, &b, &["search", "alpha_marker"]).contains("beta_marker"));
        let denied_a = global.call(
            "workspace_read",
            json!({"project_id":a_id,"rel_path":"docs/a.md"}),
        );
        assert_eq!(denied_a["result"]["isError"], true, "{state}: {denied_a}");
        assert_eq!(a_index(), original_index, "{state}");
        // The previous committed index must also remain searchable while a
        // writer owns the refresh lock. Unavailable A cannot poison this path.
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        search(&mut global);
        conn.execute_batch("ROLLBACK").unwrap();
        assert_eq!(a_index(), original_index);
        match state {
            "moved" => std::fs::rename(&moved, &a).unwrap(),
            "missing-marker" => {
                std::fs::rename(a.join(".saved-id"), a.join(".ai-workspace-id")).unwrap()
            }
            _ => std::fs::write(a.join(".ai-workspace-id"), &marker).unwrap(),
        }
    }
}

#[test]
fn codegraph_visibility_is_applied_before_the_result_limit() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let project = root.path().join("p");
    std::fs::create_dir(&project).unwrap();
    // More hidden matches than the MCP oversampling cap, all ranked before
    // the shared symbol. A larger fetch limit is not a visibility filter.
    let hidden = (0..220)
        .map(|n| format!("pub fn aaa_marker_{n:03}() {{}}\n"))
        .collect::<String>();
    std::fs::write(project.join("hidden.rs"), hidden).unwrap();
    std::fs::write(project.join("visible.rs"), "pub fn zzz_marker_000() {}\n").unwrap();
    ok(&db, &project, &["init", "--slug", "p"]);
    ok(&db, &project, &["share", "visible.rs"]);
    ok(&db, &project, &["codegraph", "reindex", "--full-project"]);
    let id = context(&db, &project)["project_id"].clone();
    let limited = ok(
        &db,
        &project,
        &["codegraph", "search", "marker", "--limit", "1"],
    );
    assert!(limited.contains("zzz_marker"), "{limited}");
    assert!(!limited.contains("aaa_marker"), "{limited}");
    let all = ok(
        &db,
        &project,
        &["codegraph", "search", "marker", "--limit", "20"],
    );
    assert!(
        all.contains("zzz_marker") && !all.contains("aaa_marker"),
        "{all}"
    );
    let explicit = ok(
        &db,
        &project,
        &[
            "codegraph",
            "search",
            "marker",
            "--limit",
            "1",
            "--full-project",
        ],
    );
    assert!(explicit.contains("aaa_marker"), "{explicit}");
    let mut client = Client::start(&db, &project, &[]);
    for query in [json!("marker"), json!(null)] {
        let hits = value(client.call(
            "codegraph_search",
            json!({"project_id":id,"query":query,"kind":"function","limit":1}),
        ));
        assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
        assert_eq!(hits[0]["name"], "zzz_marker_000");
    }
    ok(&db, &project, &["rm", "visible.rs"]);
    assert!(
        ok(
            &db,
            &project,
            &["codegraph", "search", "marker", "--limit", "1"]
        )
        .contains("No CodeGraph matches")
    );
    assert!(
        value(client.call(
            "codegraph_search",
            json!({"project_id":id,"query":"marker","limit":1})
        ))
        .as_array()
        .unwrap()
        .is_empty()
    );
}
