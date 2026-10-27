//! Native process tests, including the Windows CI job. No installed binary or
//! user database is used. Each fixture owns and reaps its own processes.
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_ai-workspace")
}

fn command(db: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(binary());
    command
        .current_dir(cwd)
        .env("AI_WORKSPACE_DB", db)
        .env_remove("AI_WORKSPACE_CONFIG")
        .env_remove("AI_WORKSPACE_SCOPE")
        .env_remove("AI_WORKSPACE_SCOPE_PROJECT")
        .env_remove("AI_WORKSPACE_SCOPE_GROUP")
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_FILE_WRITE")
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS")
        .env("NO_COLOR", "1");
    command
}

fn cli(db: &Path, cwd: &Path, args: &[&str]) -> String {
    let output = command(db, cwd).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn companion(db: &Path, suffix: &str) -> PathBuf {
    let mut value = db.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

struct Owner {
    child: Child,
    instance: Value,
    client: Client,
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Owner {
    fn start(db: &Path, cwd: &Path) -> Self {
        let mut child = command(db, cwd)
            .args(["serve", "--http", "--bind", "127.0.0.1:0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let client = client();
        let start = Instant::now();
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "owner exited during startup"
            );
            if let Ok(bytes) = std::fs::read(companion(db, ".instance.json"))
                && let Ok(instance) = serde_json::from_slice::<Value>(&bytes)
                && instance["pid"].as_u64() == Some(child.id() as u64)
                && client
                    .get(format!("{}status", instance["url"].as_str().unwrap()))
                    .bearer_auth(instance["token"].as_str().unwrap())
                    .send()
                    .is_ok_and(|r| r.status().is_success())
            {
                return Self {
                    child,
                    instance,
                    client,
                };
            }
            if start.elapsed() > Duration::from_secs(15) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("owner startup timed out");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn url(&self) -> String {
        format!("{}mcp", self.instance["url"].as_str().unwrap())
    }
    fn token(&self) -> &str {
        self.instance["token"].as_str().unwrap()
    }

    fn initialize(&self) -> String {
        let response = self.client.post(self.url()).bearer_auth(self.token())
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}))
            .send().unwrap().error_for_status().unwrap();
        let session = response.headers()["Mcp-Session-Id"]
            .to_str()
            .unwrap()
            .to_string();
        let body: Value = response.json().unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-11-25");
        session
    }

    fn rpc(&self, session: &str, method: &str, params: Value) -> Value {
        self.client
            .post(self.url())
            .bearer_auth(self.token())
            .header("Mcp-Session-Id", session)
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .unwrap()
    }
}

fn wait(child: &mut Child) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if start.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("child did not exit");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn one_owner_serves_multiple_http_clients_and_cli_with_one_exclusive_lock() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let mut owner = Owner::start(&db, root.path());
    let first = owner.initialize();
    let second = owner.initialize();
    assert_ne!(first, second);
    assert!(
        cli(
            &db,
            root.path(),
            &["init", "--name", "owner-test", "--slug", "owner-test"]
        )
        .contains("owner-test")
    );
    cli(
        &db,
        root.path(),
        &["note", "shared through owner", "--scope", "project"],
    );
    for session in [&first, &second] {
        let response = owner.rpc(
            session,
            "tools/call",
            json!({"name":"workspace_context","arguments":{}}),
        );
        let context: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(context["projects"][0]["slug"], "owner-test");
        let tools = owner.rpc(session, "tools/list", json!({}));
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "project_tree")
        );
    }
    let lock = rusqlite::Connection::open(companion(&db, ".owner-lock")).unwrap();
    lock.busy_timeout(Duration::ZERO).unwrap();
    assert!(lock.execute_batch("BEGIN EXCLUSIVE").is_err());
    let mut duplicate = command(&db, root.path())
        .args(["serve", "--http", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(!wait(&mut duplicate).success());
    let output = duplicate.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("already running"));
    assert!(owner.child.try_wait().unwrap().is_none());
    let status = cli(&db, root.path(), &["local-status"]);
    assert!(status.contains(&owner.child.id().to_string()));
}

#[test]
fn owner_recovers_after_process_death_and_stale_discovery_without_affecting_other_databases() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let mut first = Owner::start(&db, root.path());
    let old_pid = first.child.id();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let second = Owner::start(&db, root.path());
    assert_ne!(second.child.id(), old_pid);
    assert_eq!(second.instance["token"], first.instance["token"]);
    let other = Owner::start(&root.path().join("other.db"), root.path());
    assert_ne!(second.instance["url"], other.instance["url"]);
    second.initialize();
    other.initialize();
}

#[test]
fn local_http_rejects_unauthenticated_foreign_origin_and_invalid_sessions() {
    let root = tempfile::tempdir().unwrap();
    let owner = Owner::start(&root.path().join("workspace.db"), root.path());
    assert_eq!(
        owner
            .client
            .post(owner.url())
            .json(&json!({}))
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        owner
            .client
            .post(owner.url())
            .bearer_auth(owner.token())
            .header("Origin", "https://foreign.example")
            .json(&json!({}))
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        owner
            .client
            .post(owner.url())
            .bearer_auth(owner.token())
            .header("Host", "foreign.example")
            .json(&json!({}))
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        owner
            .client
            .post(owner.url())
            .bearer_auth(owner.token())
            .header("Mcp-Session-Id", "missing")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .send()
            .unwrap()
            .status(),
        404
    );
}

#[test]
fn http_resource_subscriptions_receive_cli_changes_from_the_same_owner() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let owner = Owner::start(&db, root.path());
    cli(
        &db,
        root.path(),
        &["init", "--name", "events", "--slug", "events"],
    );
    let session = owner.initialize();
    assert!(
        owner.rpc(
            &session,
            "resources/subscribe",
            json!({"uri":"workspace://events"})
        )["error"]
            .is_null()
    );
    let stream = owner
        .client
        .get(owner.url())
        .bearer_auth(owner.token())
        .header("Mcp-Session-Id", &session)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(
        stream.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    cli(
        &db,
        root.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "events",
        ],
    );
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if let Some(data) = line.strip_prefix("data: ") {
            let message: Value = serde_json::from_str(data).unwrap();
            assert_eq!(message["method"], "notifications/resources/updated");
            assert_eq!(message["params"]["uri"], "workspace://events");
            break;
        }
    }
}

struct Bridge {
    child: Child,
    input: Option<std::process::ChildStdin>,
    output: std::sync::mpsc::Receiver<Value>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Bridge {
    fn start(db: &Path, cwd: &Path, args: &[&str], write: bool) -> Self {
        let mut child = command(db, cwd)
            .arg("serve")
            .args(args)
            .env(
                "AI_WORKSPACE_ALLOW_PROJECT_FILE_WRITE",
                if write { "1" } else { "0" },
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let value = serde_json::from_str(&line).unwrap();
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
        }
    }
    fn rpc(&mut self, method: &str) -> Value {
        self.request(method, Value::Null)
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        writeln!(
            self.input.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
        )
        .unwrap();
        self.output
            .recv_timeout(Duration::from_secs(15))
            .expect("MCP bridge response timed out")
    }
    fn eof(&mut self) {
        drop(self.input.take());
        assert!(wait(&mut self.child).success());
    }
}

#[test]
fn forwarded_cli_and_current_project_bridges_use_each_callers_directory() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let _owner = Owner::start(&db, root.path());
    let left = root.path().join("left");
    let right = root.path().join("right");
    std::fs::create_dir(&left).unwrap();
    std::fs::create_dir(&right).unwrap();
    for (cwd, slug) in [(&left, "left"), (&right, "right")] {
        cli(&db, cwd, &["init", "--name", slug, "--slug", slug]);
        assert!(cli(&db, cwd, &["status"]).contains(&format!("Slug: {slug}")));
    }
    let mut first = Bridge::start(&db, &left, &["--scope", "current-project"], false);
    let mut second = Bridge::start(&db, &right, &["--scope", "current-project"], false);
    for slug in ["left", "right", "left"] {
        let bridge = if slug == "left" {
            &mut first
        } else {
            &mut second
        };
        let response = bridge.request(
            "tools/call",
            json!({"name":"workspace_context","arguments":{}}),
        );
        let context: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        let projects = context["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0]["slug"], slug);
    }
    first.eof();
    second.eof();
}

#[cfg(windows)]
#[test]
fn windows_database_path_alias_cannot_start_a_second_owner() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    let mut owner = Owner::start(&db, root.path());
    let alias = root.path().join(".").join("WORKSPACE.DB");
    let mut duplicate = command(&alias, root.path())
        .args(["serve", "--http", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(!wait(&mut duplicate).success());
    assert!(owner.child.try_wait().unwrap().is_none());
}

#[test]
fn simultaneous_stdio_launches_join_one_owner_and_keep_client_permissions_separate() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("workspace.db");
    // Start both before either has initialized: this exercises owner election.
    let mut first = Bridge::start(&db, root.path(), &[], true);
    let mut second = Bridge::start(&db, root.path(), &[], false);
    assert_eq!(
        first.rpc("initialize")["result"]["serverInfo"]["name"],
        "ai-workspace"
    );
    assert_eq!(
        second.rpc("initialize")["result"]["serverInfo"]["name"],
        "ai-workspace"
    );
    let instance: Value =
        serde_json::from_slice(&std::fs::read(companion(&db, ".instance.json")).unwrap()).unwrap();
    assert_ne!(instance["pid"].as_u64(), Some(first.child.id() as u64));
    assert_ne!(instance["pid"].as_u64(), Some(second.child.id() as u64));
    for (bridge, allowed) in [(&mut first, true), (&mut second, false)] {
        let tools = bridge.rpc("tools/list");
        assert_eq!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "project_file_write"),
            allowed
        );
    }
    first.eof();
    assert!(second.rpc("tools/list")["result"].is_object());
    second.eof();
    // Auto-started owners must release files after the last client leaves.
    let start = Instant::now();
    while companion(&db, ".instance.json").exists() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "idle owner leaked"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
