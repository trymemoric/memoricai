//! Runtime configuration from environment.

pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub ingest_concurrency: usize,
    pub production: bool,
    pub max_inflight_requests: usize,
    pub request_body_timeout: std::time::Duration,
    pub analytics_retention_days: i64,
    pub router_allowed_origins: Vec<String>,
    /// Master credential for `POST /v1/admin/provision`. Unset = endpoint disabled (404).
    pub provision_key: Option<String>,
}

fn env_any(keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()))
}

fn bounded_usize(name: &str, default: usize, min: usize, max: usize) -> anyhow::Result<usize> {
    let Some(raw) = env_any(&[name]) else {
        return Ok(default);
    };
    let value = raw
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("{name} must be an integer"))?;
    if !(min..=max).contains(&value) {
        anyhow::bail!("{name} must be between {min} and {max}");
    }
    Ok(value)
}

fn production_mode_from(value: Option<&str>, debug_assertions: bool) -> anyhow::Result<bool> {
    let Some(value) = value else {
        // Release binaries are secure-by-default; debug builds retain a frictionless local
        // development path unless the environment explicitly selects production.
        return Ok(!debug_assertions);
    };
    match value.to_ascii_lowercase().as_str() {
        "prod" | "production" => Ok(true),
        "dev" | "development" | "local" | "test" => Ok(false),
        _ => anyhow::bail!("MEMORICAI_ENV must be production/prod or development/dev/local/test"),
    }
}

fn production_mode() -> anyhow::Result<bool> {
    let value = env_any(&["MEMORICAI_ENV", "MEMORICAI_ENVIRONMENT"]);
    production_mode_from(value.as_deref(), cfg!(debug_assertions))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env_any(&["MEMORICAI_DATABASE_URL", "DATABASE_URL"])
            .ok_or_else(|| anyhow::anyhow!("MEMORICAI_DATABASE_URL is required"))?;
        let bind = env_any(&["MEMORICAI_BIND"]).unwrap_or_else(|| "0.0.0.0:7373".into());
        // The ingest pipeline is I/O-bound (model calls + per-fact DB writes),
        // so scale the default worker pool with the machine instead of a
        // fixed low constant.
        let default_ingest_concurrency = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8);
        let ingest_concurrency = bounded_usize(
            "MEMORICAI_INGEST_CONCURRENCY",
            default_ingest_concurrency,
            1,
            64,
        )?;
        let production = production_mode()?;
        let max_inflight_requests =
            bounded_usize("MEMORICAI_MAX_INFLIGHT_REQUESTS", 256, 1, 10_000)?;
        let request_body_timeout = std::time::Duration::from_secs(bounded_usize(
            "MEMORICAI_REQUEST_BODY_TIMEOUT_SECONDS",
            30,
            1,
            300,
        )? as u64);
        let analytics_retention_days =
            bounded_usize("MEMORICAI_ANALYTICS_RETENTION_DAYS", 90, 1, 3650)? as i64;
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
        let provision_key = env_any(&["MEMORICAI_PROVISION_KEY"]);
        Ok(Self {
            database_url,
            bind,
            ingest_concurrency,
            production,
            max_inflight_requests,
            request_body_timeout,
            analytics_retention_days,
            router_allowed_origins,
            provision_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::production_mode_from;

    #[test]
    fn release_defaults_to_production_and_debug_defaults_to_development() {
        assert!(production_mode_from(None, false).unwrap());
        assert!(!production_mode_from(None, true).unwrap());
    }

    #[test]
    fn explicit_environment_overrides_build_profile() {
        assert!(production_mode_from(Some("production"), true).unwrap());
        assert!(!production_mode_from(Some("development"), false).unwrap());
        assert!(production_mode_from(Some("staging"), false).is_err());
    }
}
