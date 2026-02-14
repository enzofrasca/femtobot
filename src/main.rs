mod agent;
mod bus;
mod config;
mod configure;
mod cron;
mod discord;
mod memory;
mod providers;
mod session_compaction;
mod skillhub;
mod skills;
mod skills_cli;
mod telegram;
mod tools;
mod transcription;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use tokio::io::{self, AsyncBufReadExt};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "femtobot", version, about = "femtobot CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    Run,
    Tui,
    Configure,
    Skills {
        #[command(subcommand)]
        command: skills_cli::SkillsCommands,
    },
    Cron {
        /// Admin cron operations (tool-driven scheduling is preferred)
        #[command(subcommand)]
        command: CronCommands,
    },
}

#[derive(Subcommand)]
enum CronCommands {
    List,
    Status,
    Remove {
        #[arg(long)]
        id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Run) {
        Commands::Run => run().await,
        Commands::Tui => run_tui().await,
        Commands::Configure => configure::run(),
        Commands::Skills { command } => {
            tokio::task::spawn_blocking(move || skills_cli::handle_skills(command))
                .await
                .map_err(|err| anyhow!("skills command task failed: {err}"))?
        }
        Commands::Cron { command } => handle_cron(command).await,
    }
}

async fn run() -> Result<()> {
    let cfg = config::AppConfig::load()?;

    let bus = bus::MessageBus::new();

    // Start Cron Service
    let cron_service = cron::CronService::new(&cfg, bus.clone());
    cron_service.start().await;

    let agent = agent::AgentLoop::new(cfg.clone(), bus.clone(), cron_service.clone());
    tokio::spawn(async move {
        agent.run().await;
    });

    let mut enabled_channels = 0usize;

    if cfg.telegram_enabled() {
        enabled_channels += 1;
        let telegram_cfg = cfg.clone();
        let telegram_bus = bus.clone();
        tokio::spawn(async move {
            if let Err(err) = telegram::start(telegram_cfg, telegram_bus).await {
                warn!("telegram disabled: {err}");
            }
        });
    } else {
        info!("Telegram token not configured; running without Telegram input/output");
        info!("Set TELOXIDE_TOKEN or channels.telegram.token to enable Telegram");
    }

    if cfg.discord_enabled() {
        enabled_channels += 1;
        let discord_cfg = cfg.clone();
        let discord_bus = bus.clone();
        tokio::spawn(async move {
            if let Err(err) = discord::start(discord_cfg, discord_bus).await {
                warn!("discord disabled: {err}");
            }
        });
    } else {
        info!("Discord token not configured; running without Discord input/output");
        info!("Set DISCORD_BOT_TOKEN or channels.discord.token to enable Discord");
    }

    if enabled_channels == 0 {
        warn!("femtobot is running without chat input/output; press Ctrl+C to exit");
    }
    wait_for_shutdown().await?;

    Ok(())
}

async fn wait_for_shutdown() -> Result<()> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn handle_cron(cmd: CronCommands) -> Result<()> {
    let cfg = config::AppConfig::load()?;
    // We don't need a real bus for CLI operations acting on the store
    let bus = bus::MessageBus::new();
    let service = cron::CronService::new(&cfg, bus);

    match cmd {
        CronCommands::List => {
            let jobs = service.list_jobs().await?;
            if jobs.is_empty() {
                println!("No cron jobs found.");
            } else {
                println!(
                    "{:<10} {:<20} {:<20} {:<10} {:<20}",
                    "ID", "Name", "Schedule", "Status", "Next Run"
                );
                println!("{:-<80}", "");
                for job in jobs {
                    let next = job
                        .state
                        .next_run_at_ms
                        .map(|ms| {
                            chrono::DateTime::<chrono::Utc>::from(
                                std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64),
                            )
                            .to_rfc3339()
                        })
                        .unwrap_or_else(|| "N/A".to_string());
                    let schedule_str = if job.schedule.kind == "every" {
                        format!("every {}ms", job.schedule.every_ms.unwrap_or(0))
                    } else if job.schedule.kind == "at" {
                        "at specific time".to_string()
                    } else {
                        job.schedule.expr.clone().unwrap_or("?".to_string())
                    };

                    println!(
                        "{:<10} {:<20} {:<20} {:<10} {:<20}",
                        job.id,
                        job.name,
                        schedule_str,
                        if job.enabled { "Enabled" } else { "Disabled" },
                        next
                    );
                }
            }
        }
        CronCommands::Status => {
            let status = service.status().await?;
            let next = status
                .next_wake_at_ms
                .map(|ms| {
                    chrono::DateTime::<chrono::Utc>::from(
                        std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64),
                    )
                    .to_rfc3339()
                })
                .unwrap_or_else(|| "N/A".to_string());
            println!("Jobs: {}", status.jobs);
            println!("Enabled jobs: {}", status.enabled_jobs);
            println!("Next wake: {}", next);
        }
        CronCommands::Remove { id } => match service.remove_job(&id).await {
            Ok(true) => println!("Job removed."),
            Ok(false) => println!("Job not found."),
            Err(e) => println!("Error removing job: {}", e),
        },
    }
    Ok(())
}

async fn run_tui() -> Result<()> {
    let cfg = config::AppConfig::load()?;
    let bus = bus::MessageBus::new();

    let cron_service = cron::CronService::new(&cfg, bus.clone());
    cron_service.start().await;

    let agent = agent::AgentLoop::new(cfg, bus.clone(), cron_service);
    tokio::spawn(async move {
        agent.run().await;
    });

    let bus_for_outbound = bus.clone();
    tokio::spawn(async move {
        let mut outbound_rx = bus_for_outbound.subscribe_outbound();
        loop {
            let msg = match outbound_rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            };
            if msg.channel != "tui" {
                continue;
            }
            println!("\nassistant> {}\n", msg.content.trim());
        }
    });

    println!("femtobot TUI mode");
    println!("Type messages and press Enter. Type /exit to quit.\n");

    let mut lines = io::BufReader::new(io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let content = line.trim().to_string();
        if content.is_empty() {
            continue;
        }
        if content == "/exit" {
            break;
        }
        bus.publish_inbound(bus::InboundMessage {
            channel: "tui".to_string(),
            chat_id: "local".to_string(),
            sender_id: "local".to_string(),
            content,
        })
        .await;
    }

    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
