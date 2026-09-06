use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn local_stdio_mcp_remains_available_with_its_local_tools() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-workspace"))
        .arg("serve")
        .env("AI_WORKSPACE_DB", temp.path().join("workspace.db"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list"}}"#).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let names = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"workspace_context"));
    assert!(names.contains(&"project_tree"));
    assert!(names.contains(&"codegraph_search"));
    assert!(
        names.len() > 7,
        "local stdio catalog must not be replaced by hosted allowlist"
    );
}
