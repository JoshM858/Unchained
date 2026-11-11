use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use unchained_core::{Mode, Policy, SessionManager, StubModel, ToolRegistry, Workspace};
use unchained_server::{load_or_create_token, AppState};

#[derive(Parser)]
#[command(
    name = "unchained",
    version,
    about = "An always-on, self-hosted coding agent"
)]
struct Cli {
    /// Where sessions and the access token are stored.
    #[arg(long, global = true, env = "UNCHAINED_DATA_DIR")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon and serve the web UI.
    Serve {
        /// A directory the agent may work in, as PATH or NAME=PATH. Repeatable.
        #[arg(short, long = "workspace", required = true, value_name = "[NAME=]PATH")]
        workspaces: Vec<String>,

        #[arg(long, default_value = "127.0.0.1:7878")]
        bind: SocketAddr,

        /// Permit binding a non-loopback address.
        #[arg(long)]
        allow_remote: bool,

        /// Permission mode: plan, ask, auto-edit or full-auto.
        #[arg(long, default_value = "ask")]
        mode: Mode,
    },
    /// Print the access token used to sign in.
    Token,
}

fn default_data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("unchained"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot find a home directory; pass --data-dir")?;
    Ok(PathBuf::from(home).join(".local/share/unchained"))
}

fn parse_workspace(spec: &str) -> Result<Workspace> {
    let (name, path) = match spec.split_once('=') {
        Some((name, path)) if !name.is_empty() => (name.to_string(), PathBuf::from(path)),
        _ => {
            let path = PathBuf::from(spec);
            let name = path
                .canonicalize()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .with_context(|| format!("cannot derive a name for workspace '{spec}'"))?;
            (name, path)
        }
    };
    let workspace = Workspace::new(name, &path)?;
    if !workspace.root().is_dir() {
        bail!(
            "workspace {} is not a directory",
            workspace.root().display()
        );
    }
    Ok(workspace)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let data_dir = match cli.data_dir {
        Some(dir) => dir,
        None => default_data_dir()?,
    };

    match cli.command {
        Command::Token => {
            println!("{}", load_or_create_token(&data_dir)?);
        }
        Command::Serve {
            workspaces,
            bind,
            allow_remote,
            mode,
        } => {
            let workspaces = workspaces
                .iter()
                .map(|s| parse_workspace(s))
                .collect::<Result<Vec<_>>>()?;
            let mut names = HashSet::new();
            for w in &workspaces {
                if !names.insert(w.name()) {
                    bail!("two workspaces are named '{}'; use NAME=PATH", w.name());
                }
            }
            let token = load_or_create_token(&data_dir)?;
            let manager = SessionManager::open(
                &data_dir,
                workspaces,
                Policy::new(mode),
                Arc::new(StubModel),
                Arc::new(ToolRegistry::builtin()),
            )?;
            for w in manager.workspaces() {
                tracing::info!("workspace {} -> {}", w.name, w.root);
            }
            tracing::info!("sign in with the token from `unchained token`");
            let state = AppState {
                manager: Arc::new(manager),
                token: token.into(),
            };
            unchained_server::serve(state, bind, allow_remote).await?;
        }
    }
    Ok(())
}
