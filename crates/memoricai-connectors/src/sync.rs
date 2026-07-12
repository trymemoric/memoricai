//! Shared sync engine: wrap each provider import in a SyncRun ledger entry.

use crate::{Connector, ImportCtx};
use memoricai_core::error::{Error, Result};
use memoricai_db::Db;

/// Classify an error for the SyncRun `error_kind` column.
fn error_kind(e: &Error) -> &'static str {
    match e {
        Error::Unauthorized(_) | Error::Forbidden(_) => "needs-reauth",
        Error::RateLimited => "rate-limited",
        Error::BadRequest(_) => "bad-request",
        _ => "error",
    }
}

/// Run a provider import inside a SyncRun, recording success/failure.
pub async fn run(
    db: &Db,
    connection_id: &str,
    trigger: &str,
    connector: &dyn Connector,
    ctx: &ImportCtx<'_>,
) -> Result<()> {
    let run_id = db.start_sync_run(connection_id, trigger).await?;
    let heartbeat_db = db.clone();
    let heartbeat_run_id = run_id.clone();
    let heartbeat = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.tick().await;
        loop {
            interval.tick().await;
            if heartbeat_db
                .renew_sync_run(&heartbeat_run_id)
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let import_result = connector.import(ctx).await;
    heartbeat.abort();
    let _ = heartbeat.await;
    match import_result {
        Ok(stats) => {
            // Deletion reconciliation: only when the connector fully enumerated the source
            // (opt-in + not truncated), remove documents no longer present upstream. An empty
            // seen set is a valid complete enumeration and therefore deletes all old rows.
            if stats.reconcile_deletions && !stats.truncated {
                let seen: Vec<String> = ctx.seen.lock().unwrap().iter().cloned().collect();
                if let Err(error) = db
                    .reconcile_connection_documents(&ctx.org_id, connection_id, &seen)
                    .await
                {
                    db.finish_sync_run(
                        &run_id,
                        "failed",
                        stats.processed,
                        stats.failed,
                        Some(&error.to_string()),
                        Some("reconciliation"),
                    )
                    .await?;
                    return Err(error);
                }
            }
            if let Err(error) = db
                .set_connection_synced(connection_id, stats.cursor.as_deref())
                .await
            {
                db.finish_sync_run(
                    &run_id,
                    "failed",
                    stats.processed,
                    stats.failed,
                    Some(&error.to_string()),
                    Some("checkpoint"),
                )
                .await?;
                return Err(error);
            }
            db.finish_sync_run(
                &run_id,
                "completed",
                stats.processed,
                stats.failed,
                None,
                None,
            )
            .await?;
            Ok(())
        }
        Err(e) => {
            let kind = error_kind(&e);
            db.finish_sync_run(&run_id, "failed", 0, 0, Some(&e.to_string()), Some(kind))
                .await?;
            Err(e)
        }
    }
}
