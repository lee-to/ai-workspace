use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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

fn command(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-workspace"));
    command
        .env("AI_WORKSPACE_DB", db)
        .env_remove("AI_WORKSPACE_SCOPE")
        .env_remove("AI_WORKSPACE_SCOPE_PROJECT")
        .env_remove("AI_WORKSPACE_SCOPE_GROUP");
    command
}

fn cli(db: &Path, dir: &Path, args: &[&str]) {
    let output = command(db).current_dir(dir).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
                let Ok(line) = line else {
                    break;
                };
                let message = serde_json::from_str(&line).unwrap();
                if sender.send(message).is_err() {
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

    fn receive(&self) -> Value {
        self.messages
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP message timed out")
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0", "id":7, "method":method, "params":params})
        )
        .unwrap();
        let response = self.receive();
        assert_eq!(response["id"], 7, "unexpected notification: {response}");
        response
    }

    fn quiet(&self) {
        assert!(matches!(
            self.messages.recv_timeout(Duration::from_millis(1300)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
}

fn seed(root: &Path) -> PathBuf {
    let db = root.join("events.db");
    for slug in ["auth", "billing", "unrelated"] {
        let dir = root.join(slug);
        std::fs::create_dir(&dir).unwrap();
        cli(
            &db,
            &dir,
            &["init", "--name", slug, "--slug", slug, "--group", "core"],
        );
    }
    cli(
        &db,
        root,
        &["link", "add", "billing", "auth", "--kind", "depends_on"],
    );
    db
}

#[test]
fn stdio_pushes_external_cli_changes_while_idle_and_stops_after_unsubscribe() {
    let root = tempfile::tempdir().unwrap();
    let db = seed(root.path());
    let mut client = Client::start(&db, &["--scope", "project", "--project", "billing"]);
    let init = client.request("initialize", json!({}));
    assert_eq!(
        init["result"]["capabilities"]["resources"]["subscribe"],
        true
    );
    let uri = "workspace://projects/billing/events";
    let list = client.request("resources/list", json!({}));
    let resources = list["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 2);
    assert!(resources.iter().any(|resource| resource["uri"] == uri));
    for method in ["resources/read", "resources/subscribe"] {
        let denied = client.request(
            method,
            json!({"uri":"workspace://projects/unrelated/events"}),
        );
        assert_eq!(denied["error"]["code"], -32602);
    }
    assert_eq!(
        client.request("resources/subscribe", json!({"uri":uri}))["result"],
        json!({})
    );
    client.quiet();

    // This event must neither appear in the inbox nor cause a scoped notification.
    cli(
        &db,
        root.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "unrelated",
        ],
    );
    client.quiet();
    cli(
        &db,
        root.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "auth",
        ],
    );
    assert_eq!(
        client.receive(),
        json!({"jsonrpc":"2.0", "method":"notifications/resources/updated", "params":{"uri":uri}})
    );
    let read = client.request("resources/read", json!({"uri":uri}));
    let events: Value =
        serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1);
    assert_eq!(events[0]["source_project_slug"], "auth");
    let id = events[0]["id"].to_string();
    client.quiet();

    cli(&db, root.path(), &["event", "close", &id]);
    assert_eq!(client.receive()["params"]["uri"], uri);
    let read = client.request("resources/read", json!({"uri":uri}));
    assert_eq!(read["result"]["contents"][0]["text"], "[]");
    assert_eq!(
        client.request("resources/unsubscribe", json!({"uri":uri}))["result"],
        json!({})
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
            "auth",
        ],
    );
    client.quiet();
}

#[test]
fn event_history_subscription_notifies_on_removal_and_respects_scope() {
    let root = tempfile::tempdir().unwrap();
    let db = seed(root.path());
    cli(
        &db,
        root.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "auth",
        ],
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
            "unrelated",
        ],
    );
    let mut client = Client::start(&db, &["--scope", "project", "--project", "billing"]);
    let uri = "workspace://events";
    let read = client.request("resources/read", json!({"uri":uri}));
    let events: Value =
        serde_json::from_str(read["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(events.as_array().unwrap().len(), 1);
    let id = events[0]["id"].to_string();
    assert_eq!(
        client.request("resources/subscribe", json!({"uri":uri}))["result"],
        json!({})
    );
    cli(&db, root.path(), &["event", "rm", &id]);
    assert_eq!(client.receive()["params"]["uri"], uri);
    let read = client.request("resources/read", json!({"uri":uri}));
    assert_eq!(read["result"]["contents"][0]["text"], "[]");
}
