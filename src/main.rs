mod agent;
mod archive;
mod backends;
mod deploy;
mod devices;
mod edits;
mod files;
mod flutter;
mod git;
mod inspector;
mod jobs;
mod language;
mod mcp;
mod phone_preview;
mod preview;
mod processes;
mod project_rules;
mod projects;
mod server;
mod signing;
mod stores;
#[cfg(test)]
mod tests;
mod visual;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::{
    env, io,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(version, about, subcommand_negates_reqs = true)]
struct Cli {
    #[arg(long, env = "FREEFLOW_HOME", global = true)]
    home: Option<PathBuf>,
    #[arg(
        long,
        env = "FREEFLOW_PORT",
        help = "Loopback port (default 4747, or 0 for a free port)"
    )]
    port: Option<u16>,
    #[arg(long, help = "Do not open a browser")]
    no_open: bool,
    #[arg(long, env = "FREEFLOW_ASSETS", hide = true)]
    assets: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the project-scoped MCP server on standard input and output.
    Mcp {
        #[arg(long)]
        project: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = match cli.home {
        Some(home) => std::path::absolute(home)?,
        None => dirs::home_dir()
            .context("Could not locate your home folder. Pass --home.")?
            .join(".freeflow"),
    };
    if let Some(Command::Mcp { project }) = cli.command {
        ensure!(
            project.as_deref().is_none_or(server::valid_id),
            "Use a valid Freeflow project ID."
        );
        return mcp::serve(&home, project.as_deref()).await;
    }

    if let Some((origin, is_rust)) = running(&home).await {
        ensure!(
            is_rust,
            "Another Freeflow version is running with this data folder. Close it first."
        );
        println!("Freeflow is already running at {origin}");
        if !cli.no_open {
            let _ = open::that_detached(origin);
        }
        return Ok(());
    }
    let port = cli.port.unwrap_or(4747);
    let listener = match server::bind(port).await {
        Err(error) if error.kind() == io::ErrorKind::AddrInUse && cli.port.is_none() => {
            server::bind(0).await?
        }
        result => result?,
    };
    let bin = env::current_exe()?;
    let assets = cli.assets.unwrap_or_else(|| {
        let beside_exe = bin.parent().unwrap_or(Path::new(".")).join("freeflow-web");
        if beside_exe.is_dir() {
            return beside_exe;
        }
        if let Some(parent) = bin.parent().and_then(Path::parent) {
            return parent.join("freeflow-web");
        }
        PathBuf::from("target/freeflow-web")
    });
    server::serve(listener, home, assets, bin, cli.no_open, shutdown()).await
}

async fn running(home: &Path) -> Option<(String, bool)> {
    let session: files::Session = files::read_json(&home.join("session.json"))
        .ok()
        .flatten()
        .filter(mcp::valid_session)?;
    let state: Value = mcp::client(Duration::from_millis(900))
        .get(format!("{}/api/state", session.origin))
        .header("X-Freeflow-Token", session.token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some((session.origin, state["runtime"] == "rust"))
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        return;
    }
    #[cfg(windows)]
    if let Ok(mut close) = tokio::signal::windows::ctrl_break() {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = close.recv() => {} }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}
