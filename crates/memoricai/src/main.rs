//! memoricai — single self-hostable binary: the memory & context engine + API
//! + MCP server. `memoricai serve` listens on :6767.

mod config;

use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use config::Config;
use memoricai_api::AppState;
use memoricai_auth::AuthService;
use memoricai_db::Db;
use memoricai_engine::{Engine, EngineConfig};
use memoricai_models::ModelStack;

#[derive(Parser)]
#[command(
    name = "memoricai",
    version,
    about = "Rust memory & context engine for AI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP API + MCP server.
    Serve,
    /// Apply database migrations and exit.
    Migrate,
    /// Mint a new organization + API key and print it.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
}

#[derive(Subcommand)]
enum KeyAction {
    /// Create an org + owner + API key.
    Create {
        #[arg(long, default_value = "default")]
        org_name: String,
        #[arg(long, default_value = "owner@memoricai.local")]
        email: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,memoricai=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::from_env()?;

    match cli.command {
        Command::Migrate => {
            let db = Db::connect(&config.database_url).await?;
            db.migrate().await?;
            tracing::info!("migrations applied");
        }
        Command::Key {
            action: KeyAction::Create { org_name, email },
        } => {
            let db = Db::connect(&config.database_url).await?;
            db.migrate().await?;
            let auth = AuthService::new(db);
            let (org, _user, key) = auth.bootstrap_org(&org_name, &email).await?;
            println!("organization: {} ({})", org.name, org.id);
            println!("API key (store it now, shown once):\n{key}");
        }
        Command::Serve => serve(config).await?,
    }
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    let db = Db::connect(&config.database_url).await?;
    db.migrate().await?;

    let models = Arc::new(ModelStack::from_env()?);
    tracing::info!(
        llm = %models.llm_label,
        embedder = %models.embedder_label,
        dim = models.dim(),
        "model stack"
    );

    let engine = Engine::new(
        db.clone(),
        models.clone(),
        EngineConfig {
            ingest_concurrency: config.ingest_concurrency,
            chunk_chars: 1200,
        },
    );
    let auth = Arc::new(AuthService::new(db.clone()));

    // First-run convenience: bootstrap an org + key if none exist.
    if db.count_api_keys().await.unwrap_or(0) == 0 {
        if let Ok((org, _u, key)) = auth.bootstrap_org("default", "owner@memoricai.local").await {
            tracing::warn!(org = %org.id, "no API keys found — bootstrapped a default org");
            println!("\n=== memoricai bootstrap ===");
            println!("A default organization was created. Your API key (shown once):");
            println!("  {key}");
            println!("Use it as:  Authorization: Bearer {key}\n");
        }
    }

    // Background forgetting sweeper (every minute).
    {
        let db = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                match db.sweep_forgotten().await {
                    Ok(n) if n > 0 => tracing::info!(forgotten = n, "expired memories swept"),
                    Err(e) => tracing::warn!(error = %e, "forget sweep failed"),
                    _ => {}
                }
            }
        });
    }

    // Connector sync cron (every 4 hours).
    {
        let engine = engine.clone();
        tokio::spawn(async move {
            let connectors = memoricai_connectors::Connectors::new(engine);
            let mut tick = tokio::time::interval(Duration::from_secs(4 * 3600));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                match connectors.run_due_syncs(4).await {
                    Ok(n) if n > 0 => tracing::info!(synced = n, "connector cron sync"),
                    Err(e) => tracing::warn!(error = %e, "connector cron failed"),
                    _ => {}
                }
            }
        });
    }

    // Profile `[Summary]` aggregation cron (every 6 hours).
    {
        let engine = engine.clone();
        let db = db.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(6 * 3600));
            tick.tick().await;
            loop {
                tick.tick().await;
                if let Ok(tags) = db.all_container_tags().await {
                    let mut total = 0;
                    for (org, tag) in tags {
                        if let Ok(n) = engine.aggregate_profile(&org, &tag).await {
                            total += n;
                        }
                    }
                    if total > 0 {
                        tracing::info!(summaries = total, "profile aggregation cron");
                    }
                }
            }
        });
    }

    let state = AppState {
        engine: engine.clone(),
        auth: auth.clone(),
        router_allowed_origins: Arc::new(config.router_allowed_origins),
    };
    let app = memoricai_api::build_router(state).merge(memoricai_mcp::mcp_router(engine, auth));

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, "memoricai listening");
    axum::serve(listener, app).await?;
    Ok(())
}
