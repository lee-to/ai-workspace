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
    std::fs::write(project_dir.path().join("visible.txt"), "visible_marker\n").unwrap();
    std::fs::write(project_dir.path().join(".hidden.txt"), "hidden_marker\n").unwrap();
    std::fs::write(project_dir.path().join(".env"), "secret_env_marker\n").unwrap();
    std::fs::write(
        project_dir.path().join("private.key"),
        "secret_key_marker\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join(".ssh")).unwrap();
    std::fs::write(
        project_dir.path().join(".ssh").join("id_rsa"),
        "ssh_secret_marker\n",
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

/// Seed a project with markdown content for workspace_search_fulltext policy tests.
fn seed_fulltext_policy_project(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();

    std::fs::write(
        project_dir.path().join("visible.md"),
        "visible_fulltext_marker\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join(".env.md"),
        "hidden_sensitive_fulltext_marker\n",
    )
    .unwrap();
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(
        project_dir.path().join("docs").join("public.md"),
        "directory_visible_fulltext_marker\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("docs").join("private.key.md"),
        "directory_sensitive_fulltext_marker\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("docs").join(".hidden.md"),
        "directory_hidden_fulltext_marker\n",
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
            "seed command should succeed: {:?}\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&["init", "--name", "fulltext-policy-proj"]);
    run(&["share", "visible.md"]);
    run(&["share", ".env.md"]);
    run(&["share", "docs"]);

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

    for name in ["workspace_read", "project_tree", "project_grep"] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tool should be present: {name}"));
        let properties = &tool["inputSchema"]["properties"];
        assert!(
            properties["include_hidden"].is_object(),
            "{name} should expose include_hidden"
        );
        assert!(
            properties["include_sensitive"].is_object(),
            "{name} should expose include_sensitive"
        );
    }
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
fn test_mcp_workspace_search_fulltext_hides_direct_hidden_sensitive_file() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_fulltext_policy_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search_fulltext",
                "arguments": {
                    "query": "hidden_sensitive_fulltext_marker"
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert!(
        results.is_empty(),
        "hidden/sensitive direct .md share should be filtered: {content}"
    );
}

#[test]
fn test_mcp_workspace_search_fulltext_filters_directory_hidden_sensitive_children() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_fulltext_policy_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search_fulltext",
                "arguments": {
                    "query": "directory_visible_fulltext_marker OR directory_sensitive_fulltext_marker OR directory_hidden_fulltext_marker"
                }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(
        results.len(),
        1,
        "only public directory markdown should match: {content}"
    );
    assert_eq!(results[0]["path"], "docs");
    assert!(!content.contains("directory_sensitive_fulltext_marker"));
    assert!(!content.contains("directory_hidden_fulltext_marker"));
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

// --- project_tree tests ---

#[test]
fn test_mcp_project_tree_basic() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

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
fn test_mcp_project_tree_subpath() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

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

#[test]
fn test_mcp_project_tree_hides_hidden_and_sensitive_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

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
    assert!(content.contains("main.rs"));
    assert!(content.contains("visible.txt"));
    assert!(!content.contains(".hidden.txt"));
    assert!(!content.contains(".env"));
    assert!(!content.contains(".ssh"));
    assert!(!content.contains("private.key"));
}

#[test]
fn test_mcp_project_tree_include_hidden_still_hides_sensitive() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": { "project_id": 1, "include_hidden": true }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".hidden.txt"));
    assert!(!content.contains(".env"));
    assert!(!content.contains(".ssh"));
    assert!(!content.contains("private.key"));
}

#[test]
fn test_mcp_project_tree_include_hidden_and_sensitive_shows_sensitive() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_tree_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_tree",
                "arguments": {
                    "project_id": 1,
                    "include_hidden": true,
                    "include_sensitive": true
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".env"));
    assert!(content.contains(".ssh"));
    assert!(content.contains("id_rsa"));
    assert!(content.contains("private.key"));
}

// --- workspace_read by project_id+path tests ---

#[test]
fn test_mcp_workspace_read_by_path() {
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
                "arguments": { "project_id": 1, "rel_path": "src/lib.rs" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn add"));
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

#[test]
fn test_mcp_workspace_read_by_path_blocks_hidden_sensitive_by_default() {
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
                "arguments": { "project_id": 1, "rel_path": ".env" }
            }
        })],
    );

    let result = &responses[0]["result"];
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Access denied")
    );
}

#[test]
fn test_mcp_workspace_read_by_path_allows_hidden_sensitive_with_opt_in() {
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
                "arguments": {
                    "project_id": 1,
                    "rel_path": ".env",
                    "include_hidden": true,
                    "include_sensitive": true
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(content, "secret_env_marker\n");
}

#[test]
fn test_mcp_workspace_read_directory_listing_filters_hidden_sensitive() {
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
                "arguments": { "project_id": 1, "rel_path": "." }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("main.rs"));
    assert!(content.contains("visible.txt"));
    assert!(!content.contains(".hidden.txt"));
    assert!(!content.contains(".env"));
    assert!(!content.contains(".ssh"));
    assert!(!content.contains("private.key"));
}

// --- project_grep tests ---

#[test]
fn test_mcp_project_grep_basic() {
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
                "arguments": { "project_id": 1, "pattern": "println" }
            }
        })],
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
fn test_mcp_project_grep_glob_filter() {
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
                "arguments": { "project_id": 1, "pattern": "pub fn", "glob": "*.rs" }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("pub fn"));
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

#[test]
fn test_mcp_project_grep_hides_hidden_and_sensitive_by_default() {
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
                "arguments": { "project_id": 1, "pattern": "marker" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("visible.txt"));
    assert!(!content.contains(".hidden.txt"));
    assert!(!content.contains(".env"));
    assert!(!content.contains("private.key"));
    assert!(!content.contains("id_rsa"));
}

#[test]
fn test_mcp_project_grep_include_hidden_and_sensitive_finds_sensitive() {
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
                "arguments": {
                    "project_id": 1,
                    "pattern": "secret_env_marker",
                    "include_hidden": true,
                    "include_sensitive": true
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".env"));
    assert!(content.contains("secret_env_marker"));
}
