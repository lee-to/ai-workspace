use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ai-workspace");
    path
}

fn mcp_request(db_path: &PathBuf, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut child = Command::new(binary_path())
        .arg("serve")
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start MCP server");

    let mut stdin = child.stdin.take().unwrap();
    for req in requests {
        let line = serde_json::to_string(req).unwrap();
        writeln!(stdin, "{}", line).unwrap();
    }
    drop(stdin); // Close stdin to signal EOF

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("Failed to parse response"))
        .collect()
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("workspace.db");
    (dir, db_path)
}

/// Seed a project and group via CLI before MCP tests
fn seed_data(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("readme.md"), "# Hello").unwrap();

    let run = |args: &[&str]| {
        Command::new(binary_path())
            .args(args)
            .current_dir(project_dir.path())
            .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
            .output()
            .expect("seed command failed");
    };

    run(&["init", "--name", "seed-proj", "--group", "seed-group"]);
    run(&["share", "readme.md", "--label", "readme"]);
    run(&[
        "note",
        "--group",
        "seed-group",
        "--label",
        "deploy-note",
        "This is a test note about deployment",
    ]);

    project_dir
}

#[test]
fn test_mcp_initialize() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["serverInfo"]["name"], "ai-workspace");
}

#[test]
fn test_mcp_tools_list() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        })],
    );

    assert_eq!(responses.len(), 1);
    let tools = responses[0]["result"]["tools"].as_array().unwrap();
    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    assert!(tool_names.contains(&"workspace_context"));
    assert!(tool_names.contains(&"workspace_read"));
    assert!(tool_names.contains(&"workspace_search"));
    assert!(tool_names.contains(&"list_groups"));
    assert!(tool_names.contains(&"list_projects"));
}

#[test]
fn test_mcp_workspace_context() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_context",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(content).unwrap();
    assert!(context["projects"].as_array().unwrap().len() > 0);
    assert_eq!(context["projects"][0]["name"], "seed-proj");
    // Verify labels appear in shared_items
    let shared_items = context["projects"][0]["shared_items"].as_array().unwrap();
    assert!(shared_items.iter().any(|i| i["label"] == "readme"));
}

#[test]
fn test_mcp_workspace_read() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": 1 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "# Hello");
}

#[test]
fn test_mcp_workspace_search() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search",
                "arguments": { "query": "deployment" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(results.len() > 0);
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("deployment")
    );
    // Verify label in search results
    assert_eq!(results[0]["label"], "deploy-note");
}

#[test]
fn test_mcp_list_groups() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_groups",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let groups: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(groups[0]["name"], "seed-group");
    assert!(groups[0]["projects"].as_array().unwrap().len() > 0);
}

#[test]
fn test_mcp_list_projects() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_projects",
                "arguments": {}
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let projects: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(projects[0]["name"], "seed-proj");
    assert!(projects[0]["groups"].as_array().unwrap().len() > 0);
}
