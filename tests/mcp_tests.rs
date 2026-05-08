use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target/debug/ai-workspace");
    path
}

fn mcp_request_with_env(
    db_path: &PathBuf,
    requests: &[serde_json::Value],
    envs: &[(&str, &str)],
) -> Vec<serde_json::Value> {
    let mut command = Command::new(binary_path());
    command
        .arg("serve")
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (key, value) in envs {
        command.env(key, value);
    }

    let mut child = command
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

fn mcp_request(db_path: &PathBuf, requests: &[serde_json::Value]) -> Vec<serde_json::Value> {
    mcp_request_with_env(db_path, requests, &[])
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

/// Seed a project with a file tree suitable for project_tree/project_grep tests
fn seed_tree_project(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();

    // Create file structure
    std::fs::write(
        project_dir.path().join("main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join("src")).unwrap();
    std::fs::write(
        project_dir.path().join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("src/utils.rs"),
        "pub fn greet(name: &str) {\n    println!(\"hello {}\", name);\n}\n",
    )
    .unwrap();

    // Init project via CLI
    Command::new(binary_path())
        .args(["init", "--name", "tree-proj"])
        .current_dir(project_dir.path())
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .output()
        .expect("seed command failed");

    project_dir
}

/// Seed a project with explicit shared scopes and unshared files.
fn seed_scoped_project(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();

    std::fs::write(
        project_dir.path().join("main.rs"),
        "fn main() {\n    println!(\"unshared_println_token\");\n}\n",
    )
    .unwrap();
    std::fs::write(project_dir.path().join("secret.txt"), "secret_token\n").unwrap();
    std::fs::create_dir(project_dir.path().join("src")).unwrap();
    std::fs::write(
        project_dir.path().join("src/lib.rs"),
        "pub fn shared() -> &'static str {\n    \"shared_lib_token\"\n}\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("src/private.rs"),
        "pub fn private() -> &'static str {\n    \"private_src_token\"\n}\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(
        project_dir.path().join("docs/guide.md"),
        "# Guide\n\nshared_docs_token\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("docs/notes.txt"),
        "shared_docs_text_token\n",
    )
    .unwrap();

    let run = |args: &[&str]| {
        let output = Command::new(binary_path())
            .args(args)
            .current_dir(project_dir.path())
            .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
            .output()
            .expect("seed command failed");
        assert!(
            output.status.success(),
            "seed command failed: {:?}\nstdout={}\nstderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&["init", "--name", "scoped-proj"]);
    run(&["share", "src/lib.rs", "--label", "shared-lib"]);
    run(&["share", "docs", "--label", "shared-docs"]);

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
fn test_mcp_workspace_context_hides_project_path_by_default() {
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

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(content).unwrap();
    assert!(context["projects"][0].get("path").is_none());
}

#[test]
fn test_mcp_workspace_context_includes_project_path_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(content).unwrap();
    assert!(context["projects"][0]["path"].as_str().is_some());
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
fn test_mcp_list_groups_hides_project_path_by_default() {
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

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let groups: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(groups[0]["projects"][0].get("path").is_none());
}

#[test]
fn test_mcp_list_groups_includes_project_path_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let groups: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(groups[0]["projects"][0]["path"].as_str().is_some());
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

#[test]
fn test_mcp_list_projects_hides_project_path_by_default() {
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

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let projects: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(projects[0].get("path").is_none());
}

#[test]
fn test_mcp_list_projects_includes_project_path_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let projects: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(projects[0]["path"].as_str().is_some());
}

// --- project_tree tests ---

#[test]
fn test_mcp_project_tree_basic_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request_with_env(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1 }
            }
        })],
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("main.rs"));
    assert!(content.contains("src/"));
    assert!(content.contains("lib.rs"));
    assert!(content.contains("utils.rs"));
}

#[test]
fn test_mcp_project_tree_subpath_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request_with_env(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1, "subdir": "src" }
            }
        })],
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("lib.rs"));
    assert!(content.contains("utils.rs"));
    // Should NOT contain main.rs (it's outside src/)
    assert!(!content.contains("main.rs"));
}

#[test]
fn test_mcp_project_tree_lists_only_shared_scopes_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1 }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("src/"));
    assert!(content.contains("lib.rs"));
    assert!(content.contains("docs/"));
    assert!(content.contains("guide.md"));
    assert!(content.contains("notes.txt"));
    assert!(!content.contains("main.rs"));
    assert!(!content.contains("secret.txt"));
    assert!(!content.contains("private.rs"));
}

#[test]
fn test_mcp_project_tree_subdir_filters_to_shared_descendants_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1, "subdir": "src" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("lib.rs"));
    assert!(!content.contains("private.rs"));
    assert!(!content.contains("main.rs"));
}

#[test]
fn test_mcp_project_tree_unshared_subdir_empty_by_default() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_scoped_project(&db_path);
    std::fs::create_dir(project_dir.path().join("tmp")).unwrap();
    std::fs::write(project_dir.path().join("tmp/cache.txt"), "cache").unwrap();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1, "subdir": "tmp" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "");
}

#[test]
fn test_mcp_project_tree_invalid_project() {
    let (_db_dir, db_path) = temp_db();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 9999 }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("not found"));
}

// --- workspace_read by project_id+path tests ---

#[test]
fn test_mcp_workspace_read_by_path() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request_with_env(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "src/lib.rs" }
            }
        })],
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn add"));
}

#[test]
fn test_mcp_workspace_read_by_path_denies_unshared_file_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "secret.txt" }
            }
        })],
    );

    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Access denied"));
}

#[test]
fn test_mcp_workspace_read_by_path_denies_unshared_missing_path_without_existence_leak() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "missing-secret.txt" }
            }
        })],
    );

    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Access denied"));
    assert!(!text.contains("Cannot resolve"));
}

#[test]
fn test_mcp_workspace_read_by_path_allows_shared_file() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "src/lib.rs" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("shared_lib_token"));
}

#[test]
fn test_mcp_workspace_read_by_path_allows_file_inside_shared_dir() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "docs/guide.md" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("shared_docs_token"));
}

#[test]
fn test_mcp_workspace_read_item_id_shared_dir_still_lists_children() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": 2 }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("guide.md"));
    assert!(content.contains("notes.txt"));
}

#[test]
fn test_mcp_workspace_read_path_traversal_attack() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "../../etc/passwd" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    let text = result["content"][0]["text"].as_str().unwrap();
    // Should be denied or fail to resolve
    assert!(
        text.contains("denied") || text.contains("Cannot resolve") || result["isError"] == true
    );
}

#[test]
fn test_mcp_workspace_read_missing_file() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "project_id": 1, "rel_path": "nonexistent.txt" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
}

#[test]
fn test_mcp_workspace_read_backward_compat() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_data(&db_path);

    // item_id still works
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
fn test_mcp_workspace_read_both_params_error() {
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
                "arguments": { "item_id": 1, "project_id": 1, "rel_path": "readme.md" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(responses[0]["error"].is_object());
    assert_eq!(responses[0]["error"]["code"], -32602);
}

// --- project_grep tests ---

#[test]
fn test_mcp_project_grep_basic_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request_with_env(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "println" }
            }
        })],
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("println"));
    // Should match in main.rs and src/utils.rs
    assert!(content.contains("main.rs"));
    assert!(content.contains("utils.rs"));
}

#[test]
fn test_mcp_project_grep_glob_filter_when_project_wide_enabled() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request_with_env(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "pub fn", "glob": "*.rs" }
            }
        })],
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn"));
}

#[test]
fn test_mcp_project_grep_searches_only_shared_scopes_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "token" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("src/lib.rs"));
    assert!(content.contains("shared_lib_token"));
    assert!(content.contains("docs/guide.md"));
    assert!(content.contains("shared_docs_token"));
    assert!(content.contains("docs/notes.txt"));
    assert!(content.contains("shared_docs_text_token"));
    assert!(!content.contains("main.rs"));
    assert!(!content.contains("unshared_println_token"));
    assert!(!content.contains("secret.txt"));
    assert!(!content.contains("secret_token"));
    assert!(!content.contains("private.rs"));
    assert!(!content.contains("private_src_token"));
}

#[test]
fn test_mcp_project_grep_invalid_regex() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "[invalid" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    assert!(responses[0]["error"].is_object());
    assert_eq!(responses[0]["error"]["code"], -32602);
}

#[test]
fn test_mcp_project_grep_no_matches() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "zzzzz_no_match_zzzzz" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "");
}
