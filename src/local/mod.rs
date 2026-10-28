//! Single local state owner. CLI operations acquire ownership or are forwarded;
//! stdio is a compatibility bridge, HTTP clients need no per-client process.
mod server;

use anyhow::{Context as _, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::App;
use crate::cli::{CloudCommand, Command as CliCommand};
use crate::db::owner::{self, OwnerGuard};
use crate::execution::Context;
use crate::mcp::McpScopeRequest;

#[derive(Clone, Serialize, Deserialize)]
struct Instance {
    pid: u32,
    version: String,
    url: String,
    token: String,
}

#[derive(Serialize, Deserialize)]
struct CliRequest {
    app: App,
    context: Context,
}

#[derive(Serialize, Deserialize)]
struct CliReply {
    output: String,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BridgeRequest {
    scope: McpScopeRequest,
    context: Context,
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(300))
        .build()?)
}

fn descriptor(db: &Path) -> PathBuf {
    owner::companion(db, ".instance.json")
}

fn read_instance(db: &Path) -> Result<Instance> {
    let instance: Instance = serde_json::from_slice(&std::fs::read(descriptor(db))?)?;
    let url = reqwest::Url::parse(&instance.url)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if url.scheme() != "http"
        || !loopback
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "Invalid local owner address in {}",
            descriptor(db).display()
        );
    }
    if instance.version != env!("CARGO_PKG_VERSION") {
        bail!(
            "Local owner version {} differs from CLI {}; restart the local owner",
            instance.version,
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok(instance)
}

fn ready(db: &Path) -> Result<Instance> {
    let instance = read_instance(db)?;
    let status: serde_json::Value = client()?
        .get(format!("{}status", instance.url))
        .bearer_auth(&instance.token)
        .timeout(Duration::from_secs(2))
        .send()?
        .error_for_status()?
        .json()?;
    if status["pid"].as_u64() != Some(instance.pid as u64)
        || status["version"].as_str() != Some(instance.version.as_str())
    {
        bail!("Local owner identity changed; reconnect to the workspace");
    }
    Ok(instance)
}

pub(crate) fn run(app: App) -> Result<()> {
    match &app.command {
        CliCommand::Serve {
            http,
            bind,
            idle_timeout,
            scope,
            group,
            project,
        } => {
            let request = McpScopeRequest::from_cli_and_env(
                scope.clone().map(Into::into),
                group.clone(),
                project.clone(),
            )
            .map_err(anyhow::Error::msg)?;
            if *http {
                server::serve(bind, *idle_timeout, request)
            } else {
                bridge(request)
            }
        }
        CliCommand::LocalStatus => {
            let db = owner::canonical_database_path()?;
            // A stale descriptor is never evidence of a live owner.
            let held = owner::try_acquire(&db)?;
            if held.is_some() {
                bail!("No local owner is running for {}", db.display());
            }
            let instance = ready(&db)?;
            println!(
                "Local owner PID: {}\nMCP URL: {}mcp\nCredentials file: {}",
                instance.pid,
                instance.url,
                descriptor(&db).display()
            );
            Ok(())
        }
        CliCommand::Update
        | CliCommand::Cloud {
            command: CloudCommand::Serve { .. },
        } => crate::cli::run(app.command, app.config),
        _ => run_cli(app),
    }
}

fn run_cli(app: App) -> Result<()> {
    let db = owner::canonical_database_path()?;
    let start = Instant::now();
    loop {
        if let Some(_guard) = owner::try_acquire(&db)? {
            return crate::cli::run(app.command, app.config);
        }
        if let Ok(instance) = ready(&db) {
            let request = CliRequest {
                app,
                context: Context::capture()?,
            };
            let reply: CliReply = client()?
                .post(format!("{}cli", instance.url))
                .bearer_auth(&instance.token)
                .json(&request)
                .send()?
                .error_for_status()?
                .json()?;
            print!("{}", reply.output);
            std::io::stdout().flush()?;
            return match reply.error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(()),
            };
        }
        if start.elapsed() > Duration::from_secs(10) {
            bail!(
                "Workspace is owned by another process; its local server is unavailable. Retry after that process finishes ({}).",
                db.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn start_owner(db: &Path) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "serve",
            "--http",
            "--bind",
            "127.0.0.1:0",
            "--idle-timeout",
            "5",
        ])
        .env("AI_WORKSPACE_DB", db)
        .env_remove("AI_WORKSPACE_SCOPE")
        .env_remove("AI_WORKSPACE_SCOPE_PROJECT")
        .env_remove("AI_WORKSPACE_SCOPE_GROUP")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .context("Cannot start the shared local owner")?;
    // Reap on Unix without keeping the caller alive or inheriting stdio handles.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn ensure_owner(db: &Path) -> Result<Instance> {
    let start = Instant::now();
    let mut last_spawn = None;
    loop {
        match owner::try_acquire(db)? {
            Some(guard) => {
                if last_spawn.is_none_or(|at: Instant| at.elapsed() > Duration::from_secs(2)) {
                    drop(guard);
                    start_owner(db)?;
                    last_spawn = Some(Instant::now());
                }
            }
            None => {
                if let Ok(instance) = ready(db) {
                    return Ok(instance);
                }
            }
        }
        if start.elapsed() > Duration::from_secs(15) {
            bail!(
                "Could not connect to the single local owner for {}. Another process may hold the workspace lock; try again when it finishes.",
                db.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn bridge(scope: McpScopeRequest) -> Result<()> {
    let db = owner::canonical_database_path()?;
    let instance = ensure_owner(&db)?;
    let client = client()?;
    let response = client
        .post(format!("{}bridge", instance.url))
        .bearer_auth(&instance.token)
        .json(&BridgeRequest {
            scope,
            context: Context::capture()?,
        })
        .send()?;
    let status = response.status();
    if !status.is_success() {
        bail!("Cannot connect MCP scope: {}", response.text()?);
    }
    let session: String = response.json()?;
    struct Close<'a> {
        client: &'a Client,
        instance: &'a Instance,
        session: &'a str,
    }
    impl Drop for Close<'_> {
        fn drop(&mut self) {
            let _ = self
                .client
                .delete(format!("{}mcp", self.instance.url))
                .bearer_auth(&self.instance.token)
                .header("Mcp-Session-Id", self.session)
                .timeout(Duration::from_secs(2))
                .send();
        }
    }
    let _close = Close {
        client: &client,
        instance: &instance,
        session: &session,
    };
    let (sender, receiver) = std::sync::mpsc::sync_channel(128);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let mut stdout = std::io::stdout().lock();
    let mut next_poll = Instant::now() + Duration::from_secs(1);
    loop {
        if Instant::now() >= next_poll {
            let updates: Vec<serde_json::Value> = client
                .post(format!("{}poll", instance.url))
                .bearer_auth(&instance.token)
                .header("Mcp-Session-Id", &session)
                .send()?
                .error_for_status()?
                .json()?;
            for update in updates {
                writeln!(stdout, "{update}")?;
            }
            stdout.flush()?;
            next_poll = Instant::now() + Duration::from_secs(1);
        }
        match receiver.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let response = client
                    .post(format!("{}mcp", instance.url))
                    .bearer_auth(&instance.token)
                    .header("Mcp-Session-Id", &session)
                    .header("Content-Type", "application/json")
                    .body(line)
                    .send()?;
                if response.status() != reqwest::StatusCode::ACCEPTED {
                    let response = response.error_for_status()?.text()?;
                    writeln!(stdout, "{response}")?;
                    stdout.flush()?;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

fn publish(db: &Path, instance: &Instance) -> Result<()> {
    use std::io::Write;
    let path = descriptor(db);
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing owner directory")?)?;
    serde_json::to_writer(temporary.as_file_mut(), instance)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

// A stable credential lets HTTP clients reconnect after reboot without having
// to edit every client's configuration. Only discovery metadata is ephemeral.
fn local_token(db: &Path) -> Result<String> {
    let path = owner::companion(db, ".local-token");
    match std::fs::read_to_string(&path) {
        Ok(token) if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            Ok(token)
        }
        Ok(_) => bail!("Invalid local credential file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let mut temporary =
                tempfile::NamedTempFile::new_in(path.parent().context("Missing owner directory")?)?;
            temporary.write_all(token.as_bytes())?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&path)
                .map_err(|error| error.error)?;
            Ok(token)
        }
        Err(error) => Err(error).context("Cannot read local owner credentials"),
    }
}

fn acquire_server(db: &Path) -> Result<OwnerGuard> {
    owner::try_acquire(db)?.ok_or_else(|| {
        let message = read_instance(db)
            .map(|instance| {
                format!(
                    "Local owner is already running (PID {}, {}mcp)",
                    instance.pid, instance.url
                )
            })
            .unwrap_or_else(|_| "Workspace is already owned by another process".into());
        anyhow::anyhow!(message)
    })
}
