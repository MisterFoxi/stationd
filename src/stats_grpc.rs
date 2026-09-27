//! gRPC transport for broadcast statistics (stats_v1.proto).
//!
//! Thin translator over `broadcast_log::plays`: parse the window, map the
//! grouping, read, map back. Read-only on the station database.

use tonic::{Request, Response, Status};

use crate::broadcast_log::{self, PlaysBy};

pub use crate::proto::stats;

use stats::plays_request::By;
use stats::stats_service_server::StatsService;
use stats::{PlaysRequest, PlaysResponse, PlaysRow};

/// Default window when `since` is empty.
const DEFAULT_SINCE: &str = "24h";

pub struct StatsGrpc {
    pool: sqlx::SqlitePool,
}

impl StatsGrpc {
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }
}

fn wall_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tonic::async_trait]
impl StatsService for StatsGrpc {
    async fn plays(&self, request: Request<PlaysRequest>) -> Result<Response<PlaysResponse>, Status> {
        let req = request.into_inner();
        let since = if req.since.trim().is_empty() { DEFAULT_SINCE } else { req.since.trim() };
        let secs = crate::playlist::parse_duration_secs(since)
            .map_err(|e| Status::invalid_argument(format!("since `{since}`: {e}")))?;
        let by = match By::try_from(req.by) {
            Ok(By::Playlist) => PlaysBy::Playlist,
            Ok(By::Leaf) => PlaysBy::Leaf,
            Ok(By::Rule) => PlaysBy::Rule,
            Ok(By::Origin) => PlaysBy::Origin,
            Ok(By::Media) => PlaysBy::Media,
            Ok(By::Artist) => PlaysBy::Artist,
            Err(_) => return Err(Status::invalid_argument("unknown grouping")),
        };
        let to = wall_now();
        let from = to.saturating_sub(secs as i64);
        let rows = broadcast_log::plays(&self.pool, from, by, req.limit)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        // Totals over the whole window, not just the returned lines.
        let all = broadcast_log::plays(&self.pool, from, PlaysBy::Origin, 0)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(PlaysResponse {
            from,
            to,
            total_picked: all.iter().map(|r| r.picked).sum(),
            total_aired: all.iter().map(|r| r.aired).sum(),
            rows: rows
                .into_iter()
                .map(|r| PlaysRow {
                    key: r.key.unwrap_or_default(),
                    picked: r.picked,
                    aired: r.aired,
                    last_at: r.last_at,
                })
                .collect(),
        }))
    }
}
