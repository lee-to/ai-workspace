use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::{Connection, params};

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
    mcp_request_with_options(db_path, requests, envs, &[], None)
}

fn mcp_request_with_options(
    db_path: &PathBuf,
    requests: &[serde_json::Value],
    envs: &[(&str, &str)],
    serve_args: &[&str],
    current_dir: Option<&Path>,
) -> Vec<serde_json::Value> {
    let mut command = Command::new(binary_path());
    command
        .arg("serve")
        .args(serve_args)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS")
        .env_remove("AI_WORKSPACE_SCOPE")
        .env_remove("AI_WORKSPACE_SCOPE_GROUP")
        .env_remove("AI_WORKSPACE_SCOPE_PROJECT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }

    for (key, value) in envs {
        command.env(key, value);
    }

    let mut child = command.spawn().expect("Failed to start MCP server");

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

fn run_mcp_server_without_requests(
    db_path: &PathBuf,
    envs: &[(&str, &str)],
    serve_args: &[&str],
    current_dir: Option<&Path>,
) -> std::process::Output {
    let mut command = Command::new(binary_path());
    command
        .arg("serve")
        .args(serve_args)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .env_remove("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS")
        .env_remove("AI_WORKSPACE_SCOPE")
        .env_remove("AI_WORKSPACE_SCOPE_GROUP")
        .env_remove("AI_WORKSPACE_SCOPE_PROJECT");

    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }

    for (key, value) in envs {
        command.env(key, value);
    }

    command.output().expect("failed to run MCP server")
}

fn temp_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("workspace.db");
    (dir, db_path)
}

#[cfg(unix)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

fn create_legacy_db(db_path: &PathBuf, project_path: &std::path::Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;

        CREATE TABLE projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            created_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE groups (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            created_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE project_groups (
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
            PRIMARY KEY (project_id, group_id)
        );

        CREATE TABLE shared_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL CHECK (kind IN ('file', 'dir', 'note')),
            path TEXT,
            content TEXT,
            label TEXT,
            project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
            group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
            created_by_project_id INTEGER REFERENCES projects(id),
            created_at DATETIME NOT NULL DEFAULT (datetime('now')),
            updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
            CHECK (
                (kind IN ('file', 'dir') AND path IS NOT NULL AND project_id IS NOT NULL AND content IS NULL AND group_id IS NULL)
                OR
                (kind = 'note' AND content IS NOT NULL AND project_id IS NOT NULL AND group_id IS NULL AND path IS NULL)
                OR
                (kind = 'note' AND content IS NOT NULL AND group_id IS NOT NULL AND project_id IS NULL AND path IS NULL AND created_by_project_id IS NOT NULL)
            )
        );

        CREATE VIRTUAL TABLE notes_fts USING fts5(label, content);
        CREATE VIRTUAL TABLE files_fts USING fts5(
            path,
            content,
            tokenize='unicode61 remove_diacritics 2'
        );
        CREATE TABLE files_fts_meta (
            shared_item_id INTEGER PRIMARY KEY REFERENCES shared_items(id) ON DELETE CASCADE,
            abs_path TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            indexed_at DATETIME NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX idx_shared_items_project_path
        ON shared_items (project_id, path) WHERE path IS NOT NULL;
        CREATE TRIGGER trg_shared_items_delete_fts
        AFTER DELETE ON shared_items
        BEGIN
            DELETE FROM files_fts WHERE rowid = OLD.id;
        END;
        ",
    )
    .unwrap();

    let project_path = project_path
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    conn.execute(
        "INSERT INTO projects (id, name, path) VALUES (1, 'legacy-proj', ?1)",
        params![project_path],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO groups (id, name) VALUES (1, 'legacy-group')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_groups (project_id, group_id) VALUES (1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO shared_items (id, kind, path, label, project_id) VALUES (1, 'file', 'readme.md', 'legacy-readme', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO shared_items (id, kind, content, label, group_id, created_by_project_id) VALUES (2, 'note', 'legacy note content', 'legacy-note', 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes_fts (rowid, label, content) VALUES (2, 'legacy-note', 'legacy note content')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files_fts (rowid, path, content) VALUES (1, 'readme.md', 'legacy file token')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files_fts_meta (shared_item_id, abs_path, mtime, size) VALUES (1, '/tmp/legacy/readme.md', 1, 17)",
        [],
    )
    .unwrap();
}

fn mtime_epoch(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
fn test_mcp_migrates_legacy_database_and_read_paths_work() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::write(project_dir.path().join("readme.md"), "legacy file token").unwrap();
    create_legacy_db(&db_path, project_dir.path());

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
    assert_eq!(context["projects"][0]["name"], "legacy-proj");
    assert_eq!(context["groups"][0]["name"], "legacy-group");

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "workspace_search",
                "arguments": { "query": "legacy" }
            }
        })],
    );
    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("legacy note content"));

    let conn = Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 4);
    let indexed_file_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM indexed_files", [], |row| row.get(0))
        .unwrap();
    assert_eq!(indexed_file_rows, 0);
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

fn seed_ai_factory_preset_project(db_path: &PathBuf) -> tempfile::TempDir {
    let project_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(project_dir.path().join(".ai-factory")).unwrap();
    std::fs::create_dir(project_dir.path().join(".ai-factory/references")).unwrap();
    std::fs::write(
        project_dir.path().join(".ai-factory/references/context.md"),
        "ai_factory_reference_token\n",
    )
    .unwrap();

    let output = Command::new(binary_path())
        .args([
            "init",
            "--name",
            "ai-factory-proj",
            "--preset",
            "ai-factory",
        ])
        .current_dir(project_dir.path())
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .output()
        .expect("seed command failed");
    assert!(
        output.status.success(),
        "ai-factory preset seed should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    project_dir
}

fn share_path(db_path: &PathBuf, project_dir: &Path, path: &str, label: &str) {
    let output = Command::new(binary_path())
        .args(["share", path, "--label", label])
        .current_dir(project_dir)
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .output()
        .expect("share command failed");
    assert!(
        output.status.success(),
        "share should succeed for {path}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn seed_service_event_data(db_path: &PathBuf) -> (tempfile::TempDir, tempfile::TempDir) {
    let auth_dir = tempfile::tempdir().unwrap();
    let api_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(api_dir.path().join("docs")).unwrap();
    std::fs::write(
        api_dir.path().join("docs/auth.md"),
        "Auth integration notes",
    )
    .unwrap();

    let run = |dir: &std::path::Path, args: &[&str]| {
        let output = Command::new(binary_path())
            .args(args)
            .current_dir(dir)
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

    run(
        auth_dir.path(),
        &[
            "init", "--name", "Auth", "--slug", "auth", "--group", "platform",
        ],
    );
    run(
        api_dir.path(),
        &[
            "init", "--name", "API", "--slug", "api", "--group", "platform",
        ],
    );
    run(
        api_dir.path(),
        &["share", "docs/auth.md", "--label", "auth-doc"],
    );
    run(
        api_dir.path(),
        &[
            "link",
            "add",
            "api",
            "auth",
            "--kind",
            "depends_on",
            "--label",
            "JWT",
        ],
    );
    run(
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "docs/auth.md",
            "auth",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    run(
        api_dir.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "auth",
            "--severity",
            "warning",
            "--title",
            "Auth changed",
        ],
    );

    (auth_dir, api_dir)
}

fn seed_mcp_scope_data(
    db_path: &PathBuf,
) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let api_dir = tempfile::tempdir().unwrap();
    let worker_dir = tempfile::tempdir().unwrap();
    let web_dir = tempfile::tempdir().unwrap();

    for (dir, readme_marker, lib_marker) in [
        (
            api_dir.path(),
            "api_fulltext_marker",
            "pub fn api_scope_marker() -> &'static str { \"api_grep_marker\" }\n",
        ),
        (
            worker_dir.path(),
            "worker_fulltext_marker",
            "pub fn worker_scope_marker() -> &'static str { \"worker_grep_marker\" }\n",
        ),
        (
            web_dir.path(),
            "web_fulltext_marker",
            "pub fn web_scope_marker() -> &'static str { \"web_grep_marker\" }\n",
        ),
    ] {
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("SCOPE.md"), format!("{readme_marker}\n")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), lib_marker).unwrap();
    }

    let run = |dir: &std::path::Path, args: &[&str]| {
        let output = Command::new(binary_path())
            .args(args)
            .current_dir(dir)
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

    run(
        api_dir.path(),
        &[
            "init", "--name", "API", "--slug", "api", "--group", "backend",
        ],
    );
    run(
        api_dir.path(),
        &["share", "SCOPE.md", "--label", "api-scope"],
    );
    run(
        api_dir.path(),
        &["share", "src/lib.rs", "--label", "api-lib"],
    );
    run(
        api_dir.path(),
        &[
            "note",
            "api project note scope marker",
            "--scope",
            "project",
            "--label",
            "api-note",
        ],
    );
    run(
        api_dir.path(),
        &[
            "note",
            "backend group note scope marker",
            "--group",
            "backend",
            "--label",
            "backend-note",
        ],
    );

    run(
        worker_dir.path(),
        &[
            "init", "--name", "Worker", "--slug", "worker", "--group", "backend",
        ],
    );
    run(
        worker_dir.path(),
        &["share", "SCOPE.md", "--label", "worker-scope"],
    );
    run(
        worker_dir.path(),
        &["share", "src/lib.rs", "--label", "worker-lib"],
    );
    run(
        worker_dir.path(),
        &[
            "note",
            "worker project note scope marker",
            "--scope",
            "project",
            "--label",
            "worker-note",
        ],
    );

    run(
        web_dir.path(),
        &[
            "init", "--name", "Web", "--slug", "web", "--group", "frontend",
        ],
    );
    run(
        web_dir.path(),
        &["share", "SCOPE.md", "--label", "web-scope"],
    );
    run(
        web_dir.path(),
        &["share", "src/lib.rs", "--label", "web-lib"],
    );
    run(
        web_dir.path(),
        &[
            "note",
            "web project note scope marker",
            "--scope",
            "project",
            "--label",
            "web-note",
        ],
    );
    run(
        web_dir.path(),
        &[
            "note",
            "frontend group note scope marker",
            "--group",
            "frontend",
            "--label",
            "frontend-note",
        ],
    );

    run(
        api_dir.path(),
        &[
            "link",
            "add",
            "api",
            "worker",
            "--kind",
            "depends_on",
            "--label",
            "Worker API",
        ],
    );
    run(
        web_dir.path(),
        &[
            "link",
            "add",
            "web",
            "api",
            "--kind",
            "depends_on",
            "--label",
            "API",
        ],
    );
    run(
        api_dir.path(),
        &[
            "artifact",
            "depends",
            "src/lib.rs",
            "worker",
            "--kind",
            "references",
            "--reaction",
            "update",
        ],
    );
    run(
        worker_dir.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "worker",
            "--title",
            "Worker changed",
        ],
    );
    run(
        web_dir.path(),
        &[
            "event",
            "create",
            "--kind",
            "service_changed",
            "--source",
            "web",
            "--title",
            "Web changed",
        ],
    );

    (api_dir, worker_dir, web_dir)
}

fn project_id_by_slug(db_path: &PathBuf, slug: &str) -> i64 {
    let conn = Connection::open(db_path).unwrap();
    conn.query_row(
        "SELECT id FROM projects WHERE slug = ?1",
        params![slug],
        |row| row.get(0),
    )
    .unwrap()
}

fn add_project_to_group_by_name(db_path: &PathBuf, project_slug: &str, group_name: &str) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO groups (name) VALUES (?1)",
        params![group_name],
    )
    .unwrap();
    let project_id: i64 = conn
        .query_row(
            "SELECT id FROM projects WHERE slug = ?1",
            params![project_slug],
            |row| row.get(0),
        )
        .unwrap();
    let group_id: i64 = conn
        .query_row(
            "SELECT id FROM groups WHERE name = ?1",
            params![group_name],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO project_groups (project_id, group_id) VALUES (?1, ?2)",
        params![project_id, group_id],
    )
    .unwrap();
}

fn event_id_by_source_slug(db_path: &PathBuf, slug: &str) -> i64 {
    let conn = Connection::open(db_path).unwrap();
    conn.query_row(
        "SELECT id FROM workspace_events WHERE source_project_slug = ?1",
        params![slug],
        |row| row.get(0),
    )
    .unwrap()
}

fn tool_text(response: &serde_json::Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}

fn assert_tool_error_contains(response: &serde_json::Value, expected: &str) {
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "response was not an error: {result}"
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(expected),
        "expected error to contain '{expected}', got '{text}'"
    );
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

/// Seed a stale hidden directory child beyond workspace_search_fulltext's bounded refresh window.
fn seed_stale_directory_fulltext_beyond_refresh_window(db_path: &PathBuf) -> tempfile::TempDir {
    const FILLER_COUNT: usize = 201;

    let project_dir = tempfile::tempdir().unwrap();
    for i in 0..FILLER_COUNT {
        std::fs::write(
            project_dir.path().join(format!("filler_{i:03}.md")),
            format!("filler_{i:03}_marker\n"),
        )
        .unwrap();
    }
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(
        project_dir.path().join("docs").join("public.md"),
        "public_beyond_window_marker\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.path().join("docs").join(".hidden.md"),
        "stale_hidden_beyond_window_marker\n",
    )
    .unwrap();

    let output = Command::new(binary_path())
        .args(["init", "--name", "stale-window-proj"])
        .current_dir(project_dir.path())
        .env("AI_WORKSPACE_DB", db_path.to_string_lossy().to_string())
        .output()
        .expect("seed command failed");
    assert!(
        output.status.success(),
        "init should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let mut conn = Connection::open(db_path).unwrap();
    let project_id: i64 = conn
        .query_row(
            "SELECT id FROM projects WHERE name = 'stale-window-proj'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let tx = conn.transaction().unwrap();

    for i in 0..FILLER_COUNT {
        let rel = format!("filler_{i:03}.md");
        let abs = project_dir.path().join(&rel);
        let meta = std::fs::metadata(&abs).unwrap();
        tx.execute(
            "INSERT INTO shared_items (kind, path, project_id) VALUES ('file', ?1, ?2)",
            params![rel, project_id],
        )
        .unwrap();
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO indexed_files (shared_item_id, rel_path, abs_path, mtime, size) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                &rel,
                abs.to_string_lossy(),
                mtime_epoch(&meta),
                meta.len() as i64
            ],
        )
        .unwrap();
        let indexed_file_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO files_fts (rowid, path, content) VALUES (?1, ?2, ?3)",
            params![indexed_file_id, &rel, format!("filler_{i:03}_marker")],
        )
        .unwrap();
    }

    tx.execute(
        "INSERT INTO shared_items (kind, path, project_id) VALUES ('dir', 'docs', ?1)",
        params![project_id],
    )
    .unwrap();
    let docs_id = tx.last_insert_rowid();
    let hidden_rel = "docs/.hidden.md";
    let hidden_abs = project_dir.path().join("docs").join(".hidden.md");
    let hidden_meta = std::fs::metadata(&hidden_abs).unwrap();
    tx.execute(
        "INSERT INTO indexed_files (shared_item_id, rel_path, abs_path, mtime, size) VALUES (?1, ?2, ?3, 1, ?4)",
        params![
            docs_id,
            hidden_rel,
            hidden_abs.to_string_lossy(),
            hidden_meta.len() as i64
        ],
    )
    .unwrap();
    let hidden_indexed_file_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO files_fts (rowid, path, content) VALUES (?1, ?2, ?3)",
        params![
            hidden_indexed_file_id,
            hidden_rel,
            "stale_hidden_beyond_window_marker\n"
        ],
    )
    .unwrap();
    tx.commit().unwrap();

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
    assert!(tool_names.contains(&"workspace_service_graph"));
    assert!(tool_names.contains(&"workspace_events"));
    assert!(tool_names.contains(&"workspace_event_details"));

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
    assert_eq!(context["projects"][0]["slug"], "seed-proj");
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
fn test_mcp_service_graph_events_and_event_details() {
    let (_db_dir, db_path) = temp_db();
    let (_auth_dir, _api_dir) = seed_service_event_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "workspace_context",
                    "arguments": {}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "workspace_service_graph",
                    "arguments": { "project": "api" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "workspace_events",
                    "arguments": { "project": "api" }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);

    let context_text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(context_text).unwrap();
    let api = context["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["slug"] == "api")
        .unwrap();
    let auth_doc = api["shared_items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "docs/auth.md")
        .unwrap();
    assert_eq!(auth_doc["dependencies"][0]["service"], "auth");
    assert_eq!(auth_doc["dependencies"][0]["reaction"], "update");

    let graph_text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let graph: serde_json::Value = serde_json::from_str(graph_text).unwrap();
    assert_eq!(graph["scope"]["project"], "api");
    assert_eq!(graph["links"][0]["from"], "api");
    assert_eq!(graph["links"][0]["to"], "auth");
    assert_eq!(graph["links"][0]["kind"], "depends_on");

    let events_text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let events: serde_json::Value = serde_json::from_str(events_text).unwrap();
    assert_eq!(events[0]["source_project_slug"], "auth");
    assert_eq!(events[0]["kind"], "service_changed");
    let event_id = events[0]["id"].as_i64().unwrap();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "workspace_event_details",
                "arguments": { "event_id": event_id }
            }
        })],
    );

    assert_eq!(responses.len(), 1);
    let details_text = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let details: serde_json::Value = serde_json::from_str(details_text).unwrap();
    assert_eq!(details["event"]["id"], event_id);
    assert_eq!(details["affected_services"][0]["project"], "api");
    assert_eq!(details["affected_artifacts"][0]["path"], "docs/auth.md");
    assert_eq!(details["affected_artifacts"][0]["reaction"], "update");
}

#[test]
fn test_mcp_scope_global_returns_all_projects() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);

    let responses = mcp_request(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "workspace_context", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "workspace_search",
                    "arguments": { "query": "scope" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "workspace_search_fulltext",
                    "arguments": {
                        "query": "api_fulltext_marker OR worker_fulltext_marker OR web_fulltext_marker"
                    }
                }
            }),
        ],
    );

    let context: serde_json::Value = serde_json::from_str(tool_text(&responses[0])).unwrap();
    let project_slugs: Vec<_> = context["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["slug"].as_str().unwrap())
        .collect();
    assert!(project_slugs.contains(&"api"));
    assert!(project_slugs.contains(&"worker"));
    assert!(project_slugs.contains(&"web"));

    let search_text = tool_text(&responses[1]);
    assert!(search_text.contains("api project note scope marker"));
    assert!(search_text.contains("worker project note scope marker"));
    assert!(search_text.contains("web project note scope marker"));

    let fulltext_text = tool_text(&responses[2]);
    assert!(fulltext_text.contains("api_fulltext_marker"));
    assert!(fulltext_text.contains("worker_fulltext_marker"));
    assert!(fulltext_text.contains("web_fulltext_marker"));
}

#[test]
fn test_mcp_scope_group_filters_context_search_graph_events_and_direct_tools() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);
    let web_id = project_id_by_slug(&db_path, "web");

    let responses = mcp_request_with_options(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "workspace_context", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "list_projects", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "list_groups", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "workspace_search",
                    "arguments": { "query": "scope" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "workspace_search_fulltext",
                    "arguments": {
                        "query": "api_fulltext_marker OR worker_fulltext_marker OR web_fulltext_marker"
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "tools/call",
                "params": { "name": "workspace_service_graph", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": { "name": "workspace_events", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "project_id": web_id, "rel_path": "src/lib.rs" }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "project_tree",
                    "arguments": { "project_id": web_id }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": {
                    "name": "project_grep",
                    "arguments": { "project_id": web_id, "pattern": "web_grep_marker" }
                }
            }),
        ],
        &[],
        &["--group", "backend"],
        None,
    );

    let context: serde_json::Value = serde_json::from_str(tool_text(&responses[0])).unwrap();
    let project_slugs: Vec<_> = context["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["slug"].as_str().unwrap())
        .collect();
    assert_eq!(project_slugs.len(), 2);
    assert!(project_slugs.contains(&"api"));
    assert!(project_slugs.contains(&"worker"));
    assert!(!project_slugs.contains(&"web"));
    assert_eq!(context["groups"].as_array().unwrap().len(), 1);
    assert_eq!(context["groups"][0]["name"], "backend");

    let projects: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[1])).unwrap();
    assert_eq!(projects.len(), 2);
    assert!(projects.iter().all(|project| project["slug"] != "web"));

    let groups: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[2])).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["name"], "backend");
    assert!(
        groups[0]["projects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|project| project["slug"] != "web")
    );

    let search_text = tool_text(&responses[3]);
    assert!(search_text.contains("api project note scope marker"));
    assert!(search_text.contains("worker project note scope marker"));
    assert!(search_text.contains("backend group note scope marker"));
    assert!(!search_text.contains("web project note scope marker"));
    assert!(!search_text.contains("frontend group note scope marker"));

    let fulltext_text = tool_text(&responses[4]);
    assert!(fulltext_text.contains("api_fulltext_marker"));
    assert!(fulltext_text.contains("worker_fulltext_marker"));
    assert!(!fulltext_text.contains("web_fulltext_marker"));

    let graph: serde_json::Value = serde_json::from_str(tool_text(&responses[5])).unwrap();
    let links = graph["links"].as_array().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["from"], "api");
    assert_eq!(links[0]["to"], "worker");

    let events: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[6])).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event["source_project_slug"] == "worker")
    );
    assert!(
        events
            .iter()
            .all(|event| event["source_project_slug"] != "web")
    );

    assert_tool_error_contains(&responses[7], "outside MCP scope");
    assert_tool_error_contains(&responses[8], "outside MCP scope");
    assert_tool_error_contains(&responses[9], "outside MCP scope");
}

#[test]
fn test_mcp_scope_env_group_and_project_selectors_filter_projects() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);

    let responses = mcp_request_with_options(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "list_projects", "arguments": {} }
        })],
        &[
            ("AI_WORKSPACE_SCOPE", "group"),
            ("AI_WORKSPACE_SCOPE_GROUP", "backend"),
        ],
        &[],
        None,
    );
    let projects: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[0])).unwrap();
    let slugs: Vec<_> = projects
        .iter()
        .map(|project| project["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs.len(), 2);
    assert!(slugs.contains(&"api"));
    assert!(slugs.contains(&"worker"));
    assert!(!slugs.contains(&"web"));

    let responses = mcp_request_with_options(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "list_projects", "arguments": {} }
        })],
        &[
            ("AI_WORKSPACE_SCOPE", "project"),
            ("AI_WORKSPACE_SCOPE_PROJECT", "api"),
        ],
        &[],
        None,
    );
    let projects: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[0])).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["slug"], "api");
}

#[test]
fn test_mcp_scope_project_is_strict_and_cli_overrides_env_group() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);
    let worker_id = project_id_by_slug(&db_path, "worker");

    let responses = mcp_request_with_options(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "workspace_context", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "list_groups", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "workspace_search_fulltext",
                    "arguments": {
                        "query": "api_fulltext_marker OR worker_fulltext_marker OR web_fulltext_marker"
                    }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": { "name": "workspace_service_graph", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "project_id": worker_id, "rel_path": "src/lib.rs" }
                }
            }),
        ],
        &[
            ("AI_WORKSPACE_SCOPE", "group"),
            ("AI_WORKSPACE_SCOPE_GROUP", "frontend"),
        ],
        &["--project", "api"],
        None,
    );

    let context: serde_json::Value = serde_json::from_str(tool_text(&responses[0])).unwrap();
    assert_eq!(context["projects"].as_array().unwrap().len(), 1);
    assert_eq!(context["projects"][0]["slug"], "api");
    assert_eq!(context["groups"].as_array().unwrap().len(), 1);
    assert_eq!(context["groups"][0]["name"], "backend");
    assert_eq!(
        context["groups"][0]["projects"].as_array().unwrap().len(),
        1
    );
    assert_eq!(context["groups"][0]["projects"][0]["slug"], "api");
    let notes = context["groups"][0]["notes"].as_array().unwrap();
    assert!(notes.iter().any(|note| {
        note["preview"]
            .as_str()
            .unwrap()
            .contains("backend group note scope marker")
    }));

    let groups: Vec<serde_json::Value> = serde_json::from_str(tool_text(&responses[1])).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(groups[0]["projects"][0]["slug"], "api");

    let fulltext_text = tool_text(&responses[2]);
    assert!(fulltext_text.contains("api_fulltext_marker"));
    assert!(!fulltext_text.contains("worker_fulltext_marker"));
    assert!(!fulltext_text.contains("web_fulltext_marker"));

    let graph: serde_json::Value = serde_json::from_str(tool_text(&responses[3])).unwrap();
    assert!(graph["links"].as_array().unwrap().is_empty());
    assert_tool_error_contains(&responses[4], "outside MCP scope");
}

#[test]
fn test_mcp_scope_fulltext_applies_project_scope_before_limit() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);

    let responses = mcp_request_with_options(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search_fulltext",
                "arguments": {
                    "query": "api_fulltext_marker OR worker_fulltext_marker OR web_fulltext_marker",
                    "limit": 1
                }
            }
        })],
        &[],
        &["--project", "web"],
        None,
    );

    let fulltext_text = tool_text(&responses[0]);
    assert!(fulltext_text.contains("web_fulltext_marker"));
    assert!(!fulltext_text.contains("api_fulltext_marker"));
    assert!(!fulltext_text.contains("worker_fulltext_marker"));
}

#[test]
fn test_mcp_scope_group_filters_project_service_graph_scope_groups() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);
    add_project_to_group_by_name(&db_path, "api", "frontend");

    let responses = mcp_request_with_options(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_service_graph",
                "arguments": { "project": "api" }
            }
        })],
        &[],
        &["--group", "backend"],
        None,
    );

    let graph: serde_json::Value = serde_json::from_str(tool_text(&responses[0])).unwrap();
    let groups = graph["scope"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["name"], "backend");
}

#[test]
fn test_mcp_scope_current_project_env_and_event_details_gate() {
    let (_db_dir, db_path) = temp_db();
    let (api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);
    let worker_event_id = event_id_by_source_slug(&db_path, "worker");
    let web_event_id = event_id_by_source_slug(&db_path, "web");

    let responses = mcp_request_with_options(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "workspace_context", "arguments": {} }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "workspace_event_details",
                    "arguments": { "event_id": worker_event_id }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "workspace_event_details",
                    "arguments": { "event_id": web_event_id }
                }
            }),
        ],
        &[("AI_WORKSPACE_SCOPE", "current-project")],
        &[],
        Some(api_dir.path()),
    );

    let context: serde_json::Value = serde_json::from_str(tool_text(&responses[0])).unwrap();
    assert_eq!(context["projects"].as_array().unwrap().len(), 1);
    assert_eq!(context["projects"][0]["slug"], "api");

    let details: serde_json::Value = serde_json::from_str(tool_text(&responses[1])).unwrap();
    assert_eq!(details["event"]["source_project_slug"], "worker");
    assert!(
        details["affected_services"]
            .as_array()
            .unwrap()
            .iter()
            .all(|target| target["project"] == "api")
    );
    assert!(
        details["affected_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["project"] == "api")
    );

    assert_tool_error_contains(&responses[2], "outside MCP scope");
}

#[test]
fn test_mcp_scope_invalid_serve_configurations_fail_startup() {
    let (_db_dir, db_path) = temp_db();
    let (_api_dir, _worker_dir, _web_dir) = seed_mcp_scope_data(&db_path);

    let output = run_mcp_server_without_requests(
        &db_path,
        &[],
        &["--scope", "global", "--group", "backend"],
        None,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("global"));
    assert!(stderr.contains("group"));

    let output = run_mcp_server_without_requests(&db_path, &[], &["--scope", "group"], None);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("group"));

    let output =
        run_mcp_server_without_requests(&db_path, &[("AI_WORKSPACE_SCOPE", "project")], &[], None);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("project"));
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
    assert_eq!(results[0]["path"], "docs/public.md");
    assert!(!content.contains("directory_sensitive_fulltext_marker"));
    assert!(!content.contains("directory_hidden_fulltext_marker"));
}

#[test]
fn test_mcp_workspace_search_fulltext_finds_ai_factory_preset_files_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_ai_factory_preset_project(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search_fulltext",
                "arguments": {
                    "query": "\"Project Description\""
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let results: Vec<serde_json::Value> = serde_json::from_str(content).unwrap();
    assert_eq!(
        results.len(),
        1,
        "ai-factory context should be searchable: {content}"
    );
    assert_eq!(results[0]["path"], ".ai-factory/DESCRIPTION.md");
}

#[test]
fn test_mcp_workspace_search_fulltext_revalidates_stale_directory_beyond_refresh_window() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_stale_directory_fulltext_beyond_refresh_window(&db_path);

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_search_fulltext",
                "arguments": {
                    "query": "stale_hidden_beyond_window_marker"
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
        "stale hidden directory child beyond refresh window should be revalidated: {content}"
    );
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
    assert_eq!(groups[0]["projects"][0]["slug"], "seed-proj");
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
    assert_eq!(projects[0]["slug"], "seed-proj");
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
fn test_mcp_project_tree_accepts_trailing_backslash_subdir() {
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
                "arguments": { "project_id": 1, "subdir": "docs\\" }
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
fn test_mcp_project_tree_lists_shared_ai_factory_dir_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_ai_factory_preset_project(&db_path);

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
                    "subdir": ".ai-factory/references"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("context.md"));
}

#[test]
fn test_mcp_project_tree_ai_factory_exception_does_not_expose_other_hidden_shared_files() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_ai_factory_preset_project(&db_path);
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(project_dir.path().join("docs/public.md"), "public\n").unwrap();
    std::fs::write(project_dir.path().join("docs/.hidden.md"), "hidden_token\n").unwrap();
    share_path(&db_path, project_dir.path(), "docs", "docs");

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
    assert!(content.contains(".ai-factory/"));
    assert!(content.contains("context.md"));
    assert!(content.contains("docs/"));
    assert!(content.contains("public.md"));
    assert!(!content.contains(".hidden.md"));
}

#[test]
fn test_mcp_project_tree_include_hidden_reveals_other_hidden_shared_files() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_ai_factory_preset_project(&db_path);
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(project_dir.path().join("docs/.hidden.md"), "hidden_token\n").unwrap();
    share_path(&db_path, project_dir.path(), "docs", "docs");

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
                    "include_hidden": true
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".hidden.md"));
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

#[test]
fn test_mcp_project_tree_hides_hidden_and_sensitive_by_default() {
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
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
fn test_mcp_workspace_read_accepts_backslash_rel_path() {
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
                "arguments": { "project_id": 1, "rel_path": "docs\\guide.md" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("shared_docs_token"));
}

#[test]
fn test_mcp_workspace_read_ai_factory_preset_item_id_is_readable_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_ai_factory_preset_project(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    let item_id: i64 = conn
        .query_row(
            "SELECT id FROM shared_items WHERE label = 'ai-factory-description'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": item_id }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("# Project Description"));
}

#[test]
fn test_mcp_workspace_read_ai_factory_preset_path_is_readable_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_ai_factory_preset_project(&db_path);

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
                    "rel_path": ".ai-factory/DESCRIPTION.md"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("# Project Description"));
}

#[test]
fn test_mcp_workspace_read_shared_ai_factory_sensitive_file_stays_blocked() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_ai_factory_preset_project(&db_path);
    std::fs::write(project_dir.path().join(".ai-factory/.env.md"), "secret").unwrap();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO shared_items (kind, path, label, project_id)
         VALUES ('file', '.ai-factory/.env.md', 'ai-factory-env', 1)",
        [],
    )
    .unwrap();
    let item_id = conn.last_insert_rowid();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": item_id }
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
fn test_mcp_workspace_read_item_id_normalizes_legacy_backslash_scope() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    let item_id: i64 = conn
        .query_row(
            "SELECT id FROM shared_items WHERE project_id = 1 AND path = 'docs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE shared_items SET path = ?1 WHERE id = ?2",
        params![r"docs\", item_id],
    )
    .unwrap();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "workspace_read",
                "arguments": { "item_id": item_id }
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

#[test]
fn test_mcp_workspace_read_missing_or_incomplete_selectors_error_before_db_open() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("missing-parent").join("workspace.db");

    let responses = mcp_request(
        &db_path,
        &[
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": {}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "project_id": 1 }
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "workspace_read",
                    "arguments": { "rel_path": "readme.md" }
                }
            }),
        ],
    );

    assert_eq!(responses.len(), 3);
    for response in responses {
        assert!(response["error"].is_object());
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(
            response["error"]["message"],
            "Invalid params: Missing required parameters: provide item_id OR project_id+rel_path"
        );
    }
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
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
fn test_mcp_project_grep_uses_legacy_backslash_shared_scope() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_scoped_project(&db_path);
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE shared_items SET path = ?1 WHERE project_id = 1 AND path = 'docs'",
        params![r"docs\"],
    )
    .unwrap();

    let responses = mcp_request(
        &db_path,
        &[serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "project_grep",
                "arguments": { "project_id": 1, "pattern": "shared_docs_token" }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("docs/guide.md"));
    assert!(content.contains("shared_docs_token"));
}

#[test]
fn test_mcp_project_grep_searches_shared_ai_factory_dir_by_default() {
    let (_db_dir, db_path) = temp_db();
    let _project_dir = seed_ai_factory_preset_project(&db_path);

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
                    "pattern": "ai_factory_reference_token"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".ai-factory/references/context.md"));
    assert!(content.contains("ai_factory_reference_token"));
}

#[test]
fn test_mcp_project_grep_ai_factory_exception_does_not_search_other_hidden_shared_files() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_ai_factory_preset_project(&db_path);
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(project_dir.path().join("docs/public.md"), "public_token\n").unwrap();
    std::fs::write(project_dir.path().join("docs/.hidden.md"), "hidden_token\n").unwrap();
    share_path(&db_path, project_dir.path(), "docs", "docs");

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
                    "pattern": "ai_factory_reference_token|hidden_token|public_token"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".ai-factory/references/context.md"));
    assert!(content.contains("ai_factory_reference_token"));
    assert!(content.contains("docs/public.md"));
    assert!(content.contains("public_token"));
    assert!(!content.contains("docs/.hidden.md"));
    assert!(!content.contains("hidden_token"));
}

#[test]
fn test_mcp_project_grep_include_hidden_searches_other_hidden_shared_files() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_ai_factory_preset_project(&db_path);
    std::fs::create_dir(project_dir.path().join("docs")).unwrap();
    std::fs::write(project_dir.path().join("docs/.hidden.md"), "hidden_token\n").unwrap();
    share_path(&db_path, project_dir.path(), "docs", "docs");

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
                    "pattern": "hidden_token",
                    "include_hidden": true
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("docs/.hidden.md"));
    assert!(content.contains("hidden_token"));
}

#[test]
fn test_mcp_project_grep_shared_dir_skips_symlink_escape_by_default() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_scoped_project(&db_path);
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "outside_secret_token\n").unwrap();
    let link_path = project_dir.path().join("docs").join("outside-link.txt");

    if let Err(err) = symlink_file(outside.path(), &link_path) {
        eprintln!(
            "skipping symlink regression test because symlink creation failed: {}",
            err
        );
        return;
    }

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
                    "pattern": "outside_secret_token"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(!content.contains("outside_secret_token"));
    assert!(!content.contains("outside-link.txt"));
}

#[test]
fn test_mcp_project_grep_shared_file_scope_does_not_become_directory_scope() {
    let (_db_dir, db_path) = temp_db();
    let project_dir = seed_scoped_project(&db_path);
    let shared_file_path = project_dir.path().join("src").join("lib.rs");

    std::fs::remove_file(&shared_file_path).unwrap();
    std::fs::create_dir(&shared_file_path).unwrap();
    std::fs::write(
        shared_file_path.join("leak.txt"),
        "mutated_file_scope_token\n",
    )
    .unwrap();

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
                    "pattern": "mutated_file_scope_token"
                }
            }
        })],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(!content.contains("mutated_file_scope_token"));
    assert!(!content.contains("src/lib.rs/leak.txt"));
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
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

    let responses = mcp_request_with_env(
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
        &[("AI_WORKSPACE_ALLOW_PROJECT_WIDE_TOOLS", "1")],
    );

    let content = responses[0]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains(".env"));
    assert!(content.contains("secret_env_marker"));
}
