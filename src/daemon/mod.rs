//! Long-running daemon for the viewer, MCP HTTP, and file watching.

use std::env;
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager, ServiceStartCtx,
    ServiceStatus, ServiceStatusCtx, ServiceUninstallCtx,
};
use tokio::sync::Mutex;

use crate::api;
use crate::config::{AppPaths, Credentials};
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

    let credentials = Credentials::load_or_create(&paths.credentials_file())?;
    let state = web::AppState::new(paths.clone(), database.clone(), port);
    let write_lock = Arc::new(Mutex::new(()));
    let api_state = api::new(paths, database.clone(), port, write_lock.clone());
    let mcp_app = Router::new()
        .nest_service("/mcp", mcp::http_service(paths.clone(), database))
        .layer(middleware::from_fn_with_state(
            credentials.mcp_token.clone(),
            require_bearer,
        ));
    let api_app = api::router(api_state).layer(middleware::from_fn_with_state(
        credentials.mcp_token.clone(),
        require_bearer,
    ));
    let app = web::router(state.clone())
        .merge(mcp_app)
        .merge(api_app)
        .layer(middleware::from_fn_with_state(state, web::guard));

    let watcher_paths = paths.clone();
    tokio::spawn(async move {
        if let Err(error) = index::watch::run(&watcher_paths, write_lock).await {
            tracing::error!("watcher stopped: {error:#}");
        }
    });

    let display_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    fs::write(
        paths.daemon_endpoint_file(),
        format!("http://{display_host}:{port}"),
    )
    .context("cannot write the daemon endpoint file")?;
    fs::write(paths.daemon_pid_file(), std::process::id().to_string())
        .context("cannot write the daemon pid file")?;
    eprintln!("Mindmount daemon listening on http://{display_host}:{port} (web and /mcp)");

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    let _ = fs::remove_file(paths.daemon_pid_file());
    let _ = fs::remove_file(paths.daemon_endpoint_file());
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
    let executable = env::current_exe().context("cannot locate the mindmount executable")?;
    let mut manager = service_manager()?;
    manager
        .set_level(ServiceLevel::User)
        .context("cannot select a user service")?;
    manager
        .install(ServiceInstallCtx {
            label: service_label(),
            program: executable,
            args: vec![
                "daemon".into(),
                "run".into(),
                "--host".into(),
                host.into(),
                "--port".into(),
                port.to_string().into(),
            ],
            contents: None,
            username: None,
            working_directory: None,
            environment: service_environment(),
            autostart: true,
            restart_policy: RestartPolicy::OnFailure {
                delay_secs: None,
                max_retries: None,
                reset_after_secs: None,
            },
        })
        .context("cannot install the daemon service")?;
    manager
        .start(ServiceStartCtx {
            label: service_label(),
        })
        .context("cannot start the daemon service")?;

    for _ in 0..50 {
        if let (Some(pid), true) = (running_pid(paths), paths.daemon_endpoint_file().is_file()) {
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
    let pid = running_pid(paths);
    let mut managed = false;
    if let Ok(mut manager) = service_manager() {
        let _ = manager.set_level(ServiceLevel::User);
        managed = matches!(
            manager.status(ServiceStatusCtx {
                label: service_label(),
            }),
            Ok(ServiceStatus::Running | ServiceStatus::Stopped(_))
        );
        let _ = manager.uninstall(ServiceUninstallCtx {
            label: service_label(),
        });
    }

    if let Some(pid) = pid {
        // Let the service manager stop the process before signaling it directly.
        if managed {
            for _ in 0..20 {
                if !is_running(pid) {
                    cleanup_daemon_files(paths);
                    println!("Stopped the Mindmount daemon.");
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        for _ in 0..50 {
            if !is_running(pid) {
                cleanup_daemon_files(paths);
                println!("Stopped the Mindmount daemon.");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!("the daemon did not stop");
    }

    cleanup_daemon_files(paths);
    println!("The Mindmount daemon is not running.");
    Ok(())
}

fn cleanup_daemon_files(paths: &AppPaths) {
    let _ = fs::remove_file(paths.daemon_pid_file());
    let _ = fs::remove_file(paths.daemon_endpoint_file());
}

pub async fn status(paths: &AppPaths) -> Result<()> {
    match service_status() {
        Some(status) => println!("Service: {status}"),
        None => println!("Service: unavailable"),
    }
    match running_pid(paths) {
        Some(pid) => {
            println!("The Mindmount daemon is running (pid {pid}).");
            match health(paths).await {
                Ok(endpoint) => println!("API is responding at {endpoint}"),
                Err(error) => println!("API is not responding: {error}"),
            }
        }
        None => {
            let _ = fs::remove_file(paths.daemon_pid_file());
            let _ = fs::remove_file(paths.daemon_endpoint_file());
            println!("The Mindmount daemon is not running.");
        }
    }
    Ok(())
}

fn service_manager() -> Result<Box<dyn ServiceManager>> {
    if env::var_os("MINDMOUNT_DISABLE_SERVICE").is_some() {
        bail!("service management is disabled by MINDMOUNT_DISABLE_SERVICE");
    }
    <dyn ServiceManager>::native().context("no supported service manager is available")
}

fn service_label() -> ServiceLabel {
    ServiceLabel {
        qualifier: Some("dev".into()),
        organization: Some("mindmount".into()),
        application: "daemon".into(),
    }
}

fn service_environment() -> Option<Vec<(String, String)>> {
    const KEYS: [&str; 6] = [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "RUST_LOG",
    ];
    let values: Vec<(String, String)> = KEYS
        .iter()
        .filter_map(|key| env::var(key).ok().map(|value| (key.to_string(), value)))
        .collect();
    (!values.is_empty()).then_some(values)
}

fn service_status() -> Option<String> {
    let mut manager = service_manager().ok()?;
    manager.set_level(ServiceLevel::User).ok()?;
    let status = manager
        .status(ServiceStatusCtx {
            label: service_label(),
        })
        .ok()?;
    Some(match status {
        ServiceStatus::NotInstalled => "not installed".into(),
        ServiceStatus::Running => "running".into(),
        ServiceStatus::Stopped(reason) => match reason {
            Some(reason) => format!("stopped ({reason})"),
            None => "stopped".into(),
        },
    })
}

async fn health(paths: &AppPaths) -> Result<String> {
    let endpoint = fs::read_to_string(paths.daemon_endpoint_file())
        .context("the daemon endpoint file is missing")?
        .trim()
        .to_string();
    let token = Credentials::load(&paths.credentials_file())?.mcp_token;
    reqwest::Client::new()
        .get(format!("{endpoint}/api/admin/status"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .context("cannot reach the daemon API")?
        .error_for_status()
        .context("the daemon API returned an error")?;
    Ok(endpoint)
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
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("cannot install the SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn require_bearer(State(token): State<String>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == token);
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response();
    }
    next.run(request).await
}
