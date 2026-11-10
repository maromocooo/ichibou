use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod doctor;

#[derive(Parser)]
#[command(name = "aperture", version, about = "See what your coding agent is building, and know when it is really done.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Project directory.
    #[arg(long, global = true, default_value = ".")]
    project: PathBuf,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the daemon for a project and open the UI (the default).
    Serve {
        /// Port to listen on. 0 picks a free one.
        #[arg(long, default_value_t = 4680)]
        port: u16,
        /// Do not open a browser.
        #[arg(long)]
        no_open: bool,
        /// Agent used for new runs: claude, codex or fake.
        #[arg(long)]
        agent: Option<String>,
        /// Script for the fake agent (tests and demos).
        #[arg(long)]
        fake_script: Option<PathBuf>,
        /// Print the URL as JSON on the first line (for tooling).
        #[arg(long)]
        json: bool,
    },
    /// Check that the tools Aperture drives are installed.
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Cmd::Serve {
        port: 4680,
        no_open: false,
        agent: None,
        fake_script: None,
        json: false,
    }) {
        Cmd::Doctor => {
            let ok = doctor::run();
            std::process::exit(if ok { 0 } else { 1 });
        }
        Cmd::Serve {
            port,
            no_open,
            agent,
            fake_script,
            json,
        } => {
            let server = aperture_server::serve(&cli.project, port).await?;
            if agent.is_some() || fake_script.is_some() {
                let mut config = server.engine.config();
                if let Some(agent) = agent {
                    config.default_agent = serde_json::from_value(serde_json::json!(agent))
                        .map_err(|_| anyhow::anyhow!("unknown agent `{agent}` (claude, codex or fake)"))?;
                }
                if let Some(script) = fake_script {
                    config.fake_script = Some(script.canonicalize()?.to_string_lossy().into_owned());
                }
                server.engine.set_config(&config)?;
            }
            let url = server.url();
            if json {
                println!("{}", serde_json::json!({ "url": url, "port": server.addr.port(), "token": server.engine.token }));
            } else {
                println!("Aperture is running for {}", server.engine.project.root.display());
                println!("  {url}");
            }
            if let Ok(Some(run)) = server.engine.resume_interrupted().await {
                eprintln!("Resumed the session that was interrupted ({})", run.id);
            }
            if !no_open && !json {
                let _ = Command::new("open").arg(&url).status();
            }
            tokio::signal::ctrl_c().await?;
            server.shutdown().await;
        }
    }
    Ok(())
}
