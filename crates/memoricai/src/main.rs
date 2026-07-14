//! memoricai — single self-hostable binary: the memory & context engine + API
//! + MCP server. `memoricai serve` listens on :7373.

mod config;

use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use config::Config;
use memoricai_api::{AnalyticsWriter, AppState};
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
    /// Install the database schema and exit.
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
    memoricai_db::crypto::validate_configuration(config.production)?;

    match cli.command {
        Command::Migrate => {
            let db = Db::connect(&config.database_url).await?;
            db.migrate().await?;
            tracing::info!("database schema ready");
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

    let auth = Arc::new(AuthService::new(db.clone()));

    let provision_key = config.provision_key.as_deref().map(Arc::from);
    if provision_key.is_some() {
        tracing::warn!("admin provisioning endpoint enabled");
    }

    // Never mint and print an owner credential implicitly in production. Operators must
    // create the first key through the explicit, auditable `key create` command.
    if db.count_api_keys().await? == 0 {
        if config.production {
            anyhow::bail!(
                "no API keys exist; run `memoricai key create` before starting production"
            );
        }
        let (org, _user, key) = auth
            .bootstrap_org("default", "owner@memoricai.local")
            .await?;
        tracing::warn!(org = %org.id, "no API keys found — bootstrapped a development org");
        println!("\n=== memoricai development bootstrap ===");
        println!("A development organization was created. API key (shown once):");
        println!("  {key}");
        println!("Use it as:  Authorization: Bearer {key}\n");
    }

    let models = Arc::new(ModelStack::from_env()?);
    tracing::info!(
        llm = %models.llm_label,
        embedder = %models.embedder_label,
        dim = models.dim(),
        embedding_provider = %models.embedding_model.provider,
        embedding_model = %models.embedding_model.model_id,
        embedding_version = %models.embedding_model.version,
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
    // Bound analytics growth. The first interval tick runs immediately at startup.
    {
        let db = db.clone();
        let retention_days = config.analytics_retention_days;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(24 * 3600));
            loop {
                tick.tick().await;
                match db.purge_request_logs(retention_days).await {
                    Ok(count) if count > 0 => {
                        tracing::info!(count, retention_days, "expired analytics logs purged")
                    }
                    Err(error) => tracing::warn!(%error, "analytics retention purge failed"),
                    _ => {}
                }
            }
        });
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
        analytics: AnalyticsWriter::new(db.clone()),
        request_body_timeout: config.request_body_timeout,
        router_allowed_origins: Arc::new(config.router_allowed_origins),
        provision_key,
    };
    let app = memoricai_api::build_router(state)
        .merge(memoricai_mcp::mcp_router(engine, auth))
        // Body-size limit and body-read timeout are applied AFTER the MCP merge so the
        // /mcp routes get the same protection as /v1 (the layers inside build_router only
        // cover routes present when they were applied). Without the timeout, /mcp is a
        // slowloris target.
        .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024))
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            config.request_body_timeout,
        ))
        .layer(axum::middleware::from_fn(memoricai_api::security_headers))
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(
            config.max_inflight_requests,
        ));

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, "memoricai listening");
    axum::serve(listener, app).await?;
    Ok(())
}
