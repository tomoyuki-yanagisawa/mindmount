//! Long-running daemon for the viewer, MCP HTTP, and file watching.

use std::fs::{self, OpenOptions};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::middleware;

use crate::config::AppPaths;
use crate::db::Database;
use crate::index;
use crate::mcp;
use crate::web;

pub async fn run(paths: &AppPaths, host: &str, port: u16) -> Result<()> {
    if !web::is_loopback(host) {
        bail!("the daemon binds only to a loopback address (127.0.0.1 or ::1)");
    }
    paths.ensure_dirs()?;
    let _daemon_lock = paths.daemon_lock()?;

    let database = Arc::new(Database::open(&paths.database_file()).await?);
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("cannot bind {host}:{port}"))?;
    let port = listener.local_addr()?.port();

    let state = web::AppState::new(paths.clone(), database.clone(), port);
    let app = web::router(state.clone())
        .nest_service("/mcp", mcp::http_service(paths.clone(), database))
        .layer(middleware::from_fn_with_state(state, web::guard));

    let watcher_paths = paths.clone();
    tokio::spawn(async move {
        if let Err(error) = index::watch::run(&watcher_paths).await {
            tracing::error!("watcher stopped: {error:#}");
        }
    });

    fs::write(paths.daemon_pid_file(), std::process::id().to_string())
        .context("cannot write the daemon pid file")?;
    let display_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    eprintln!("Mindmount daemon listening on http://{display_host}:{port} (web and /mcp)");

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    let _ = fs::remove_file(paths.daemon_pid_file());
    result.context("the daemon stopped unexpectedly")
}

pub async fn start(paths: &AppPaths, host: &str, port: u16) -> Result<()> {
    if !web::is_loopback(host) {
        bail!("the daemon binds only to a loopback address (127.0.0.1 or ::1)");
    }
    if let Some(pid) = running_pid(paths) {
        bail!("daemon is already running (pid {pid})");
    }
    paths.ensure_dirs()?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.daemon_log_file())
        .context("cannot open the daemon log")?;
    let executable = std::env::current_exe().context("cannot locate the mindmount executable")?;
    Command::new(executable)
        .args(["daemon", "run", "--host", host, "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("cannot start the daemon")?;

    for _ in 0..30 {
        if let Some(pid) = running_pid(paths) {
            println!("Started the Mindmount daemon (pid {pid})");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "the daemon did not start; see {}",
        paths.daemon_log_file().display()
    )
}

pub async fn stop(paths: &AppPaths) -> Result<()> {
    let Some(pid) = running_pid(paths) else {
        let _ = fs::remove_file(paths.daemon_pid_file());
        println!("The Mindmount daemon is not running.");
        return Ok(());
    };
    Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .context("cannot signal the daemon")?;
    for _ in 0..50 {
        if !is_running(pid) {
            let _ = fs::remove_file(paths.daemon_pid_file());
            println!("Stopped the Mindmount daemon.");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("the daemon did not stop")
}

pub async fn status(paths: &AppPaths) -> Result<()> {
    match running_pid(paths) {
        Some(pid) => println!("The Mindmount daemon is running (pid {pid})."),
        None => {
            let _ = fs::remove_file(paths.daemon_pid_file());
            println!("The Mindmount daemon is not running.");
        }
    }
    Ok(())
}

pub async fn restart(paths: &AppPaths, host: &str, port: u16) -> Result<()> {
    stop(paths).await?;
    start(paths, host, port).await
}

fn running_pid(paths: &AppPaths) -> Option<u32> {
    let pid = fs::read_to_string(paths.daemon_pid_file())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    is_running(pid).then_some(pid)
}

fn is_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
