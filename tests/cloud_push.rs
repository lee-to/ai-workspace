use rusqlite::Connection;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;
use std::thread;

fn run(project: &std::path::Path, db: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ai-workspace"))
        .args(args)
        .current_dir(project)
        .env("AI_WORKSPACE_DB", db)
        .env("RUST_LOG", "debug")
        .output()
        .unwrap()
}

#[test]
fn cloud_push_sends_policy_safe_snapshot_and_saves_revision() {
    let project = tempfile::tempdir().unwrap();
    let db = project.path().join("workspace.db");
    fs::create_dir(project.path().join("docs")).unwrap();
    fs::write(project.path().join("docs/public.md"), "public cloud marker").unwrap();
    fs::write(project.path().join("docs/.env"), "TOP_SECRET=never-send").unwrap();
    assert!(
        run(project.path(), &db, &["init", "--name", "Cloud Demo"])
            .status
            .success()
    );
    assert!(
        run(project.path(), &db, &["share", "docs"])
            .status
            .success()
    );
    assert!(
        run(
            project.path(),
            &db,
            &["note", "durable note", "--scope", "project"]
        )
        .status
        .success()
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 8192];
        let body_start = loop {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);
            if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..body_start]).into_owned();
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_owned)
            })
            .unwrap()
            .parse::<usize>()
            .unwrap();
        while request.len() - body_start < length {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);
        }
        let body: Value =
            serde_json::from_slice(&request[body_start..body_start + length]).unwrap();
        let response = json!({
            "revision": 7,
            "snapshot_hash": body["snapshot_hash"],
            "counts": {
                "groups": body["snapshot"]["groups"].as_array().unwrap().len(),
                "shares": body["snapshot"]["shares"].as_array().unwrap().len(),
                "documents": body["snapshot"]["documents"].as_array().unwrap().len(),
                "notes": body["snapshot"]["notes"].as_array().unwrap().len(),
                "service_links": body["snapshot"]["service_links"].as_array().unwrap().len(),
                "dependencies": body["snapshot"]["dependencies"].as_array().unwrap().len(),
                "events": body["snapshot"]["events"].as_array().unwrap().len()
            },
            "no_op": false
        });
        let response = serde_json::to_vec(&response).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        )
        .unwrap();
        stream.write_all(&response).unwrap();
        sender.send((headers, body)).unwrap();
    });

    let endpoint = format!("http://{address}");
    let output = Command::new(env!("CARGO_BIN_EXE_ai-workspace"))
        .args([
            "cloud",
            "push",
            "--include-markdown",
            "--url",
            &endpoint,
            "--workspace",
            "team",
        ])
        .current_dir(project.path())
        .env("AI_WORKSPACE_DB", &db)
        .env("AI_WORKSPACE_CLOUD_TOKEN", "fixture-token-never-log")
        .env("RUST_LOG", "ai_workspace::cloud=debug")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().unwrap();
    let (headers, body) = receiver.recv().unwrap();
    assert!(
        headers.contains("authorization: Bearer fixture-token-never-log")
            || headers.contains("Authorization: Bearer fixture-token-never-log")
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(serialized.contains("public cloud marker"));
    assert!(serialized.contains("durable note"));
    assert!(!serialized.contains("TOP_SECRET"));
    assert!(!serialized.contains(".env"));
    let project_path = project.path().to_string_lossy();
    assert!(!serialized.contains(project_path.as_ref()));
    assert!(!serialized.contains("\"id\":"));
    assert!(!serialized.contains("fixture-token-never-log"));
    let logs = String::from_utf8_lossy(&output.stderr);
    for forbidden in [
        "fixture-token-never-log",
        "public cloud marker",
        "durable note",
        "TOP_SECRET",
        ".env",
        project_path.as_ref(),
    ] {
        assert!(!logs.contains(forbidden), "logs contain {forbidden}");
    }

    let connection = Connection::open(db).unwrap();
    let revision: i64 = connection
        .query_row("SELECT revision FROM cloud_sync_state", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(revision, 7);
}
