//! Database access

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::prelude::FromRow;

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;

#[derive(FromRow)]
pub struct AcquiredJob {
    pub id: i64,
    pub job_data: Vec<u8>,
    pub retries: i64,
}

#[async_trait]
pub trait Database {
    async fn run_migrations(&self) -> Result<()>;
    async fn abort_jobs(&self) -> Result<()>;
    async fn acquire_job(&self) -> Result<Option<AcquiredJob>>;
    async fn delete_job(&self, id: i64) -> Result<()>;
    async fn reschedule_job(&self, run_after: DateTime<Utc>, data: Vec<u8>, id: i64) -> Result<()>;
    async fn is_task_pending(&self) -> Result<bool>;
    async fn schedule_job(
        &self,
        unique_job_id: String,
        run_after: DateTime<Utc>,
        job_data: Vec<u8>,
        job_priority: i64,
    ) -> Result<()>;
}
