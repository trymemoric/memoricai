//! Runtime configuration from environment.

pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub ingest_concurrency: usize,
    pub router_allowed_origins: Vec<String>,
}

fn env_any(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env_any(&["MEMORICAI_DATABASE_URL", "DATABASE_URL"])
            .ok_or_else(|| anyhow::anyhow!("MEMORICAI_DATABASE_URL is required"))?;
        let bind = env_any(&["MEMORICAI_BIND"]).unwrap_or_else(|| "0.0.0.0:6767".into());
        // The ingest pipeline is I/O-bound (model calls + per-fact DB writes),
        // so scale the default worker pool with the machine instead of a
        // fixed low constant.
        let default_ingest_concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        let ingest_concurrency = env_any(&["MEMORICAI_INGEST_CONCURRENCY"])
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_ingest_concurrency);
        let router_allowed_origins = env_any(&["MEMORICAI_ROUTER_ALLOWED_ORIGINS"])
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            database_url,
            bind,
            ingest_concurrency,
            router_allowed_origins,
        })
    }
}
