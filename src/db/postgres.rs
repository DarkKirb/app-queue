//! SQlite implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{query, query_as, PgPool};

use super::{AcquiredJob, Database};

#[async_trait]
impl Database for PgPool {
    async fn run_migrations(&self) -> Result<()> {
        sqlx::migrate!("migrations/sqlite")
            .run(self)
            .await
            .context("Applying migrations")?;
        Ok(())
    }

    async fn abort_jobs(&self) -> Result<()> {
        query!("UPDATE jobs SET is_running = 'f', retries = retries + 1 WHERE is_running = 't'")
            .execute(self)
            .await
            .context("Requeuing aborted jobs")?;
        Ok(())
    }

    async fn acquire_job(&self) -> Result<Option<AcquiredJob>> {
        let now = Utc::now();
        let job_info = query_as!(
            AcquiredJob,
            r#"
UPDATE jobs
    SET is_running = 't'
    WHERE id IN (
        SELECT id FROM jobs
        WHERE is_running = 'f'
        AND run_after <= $1
        ORDER BY priority DESC, run_after ASC
        LIMIT 1)
    RETURNING id, job_data, retries
        "#,
            now
        )
        .fetch_optional(self)
        .await
        .context("Loading idle queue entry from database")?;
        Ok(job_info)
    }

    async fn delete_job(&self, id: i64) -> Result<()> {
        query!("DELETE FROM jobs WHERE id = $1", id)
            .execute(self)
            .await?;
        Ok(())
    }

    async fn reschedule_job(&self, run_after: DateTime<Utc>, data: Vec<u8>, id: i64) -> Result<()> {
        query!("UPDATE jobs SET is_running = 'f', run_after = $1, retries = retries + 1, job_data = $2 WHERE id = $3", run_after, data, id)
        .execute(self)
        .await?;
        Ok(())
    }

    async fn is_task_pending(&self) -> Result<bool> {
        let now = Utc::now();
        let res = query!(
            "SELECT id FROM jobs WHERE is_running = 'f' AND run_after <= $1 LIMIT 1",
            now
        )
        .fetch_optional(self)
        .await?;
        Ok(res.is_some())
    }

    async fn schedule_job(
        &self,
        unique_job_id: String,
        run_after: DateTime<Utc>,
        job_data: Vec<u8>,
        job_priority: i64,
    ) -> Result<()> {
        query!(
            "INSERT INTO jobs (unique_job_id, run_after, job_data, priority) VALUES ($1, $2, $3, $4)",
            unique_job_id,
            run_after,
            job_data,
            job_priority
        )
        .execute(self)
        .await?;
        Ok(())
    }
}
