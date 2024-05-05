//! In-app persistent queue
//!
//! This crate implements a job queue for monolithic applications that persists across application restarts.
//!
//! Thanks to the [`typetag`](https://crates.io/crates/typetag) crate, your [`Job`](trait.Job.html)s can use any serializable data type.
//!
//! ```
//! # use serde::{Deserialize, Serialize};
//! # use anyhow::Result;
//! # use std::sync::Arc;
//! # use app_queue::{AppQueue, Job};
//! # static NOTIFIER: tokio::sync::Notify = tokio::sync::Notify::const_new();
//! #[derive(Clone, Debug, Serialize, Deserialize)]
//! pub struct MyJob {
//!     message: String
//! }
//!
//! #[typetag::serde]
//! #[async_trait::async_trait]
//! impl Job for MyJob {
//!   async fn run(&mut self, _: Arc<AppQueue>) -> Result<()> {
//!     println!("{}", self.message);
//! #    NOTIFIER.notify_one();
//!     Ok(())
//!   }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//! # tracing_subscriber::fmt::init();
//!   let queue = AppQueue::new("/tmp/queue.db").await?;
//!   let job = MyJob {
//!     message: "Hello, world!".into()
//!   };
//!   queue.add_job(Box::new(job)).await?;
//!   queue.run_job_workers_default();
//! # NOTIFIER.notified().await;
//!   Ok(())
//! }
//! ```

use std::{convert::Infallible, path::Path, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{query, sqlite::SqliteConnectOptions, SqlitePool};
use tokio::sync::Notify;
use tracing::{debug, error, info};
use uuid::Uuid;

/// The Never `!` type. Can’t be created. Indicates that the function will never return.
pub type Never = Infallible;

/// Queued Job interface
#[typetag::serde(tag = "type")]
#[async_trait]
pub trait Job: Send + Sync {
    /// Attempt to run the job.
    ///
    /// # Errors
    ///
    /// In addition to any errors caused by the job itself, the job may return an error to indicate that the job should be requeued.
    async fn run(&mut self, queue: Arc<AppQueue>) -> Result<()>;

    /// Check if an error is fatal.
    ///
    /// Jobs that return a fatal error will not be requeued.
    ///
    /// The default implementation treats all errors as non-fatal.
    fn is_fatal_error(&self, _: &anyhow::Error) -> bool {
        false
    }

    /// Calculates the next time the job should be retried.
    ///
    /// It receives the number of times the job has already been retried.
    ///
    /// The default behavior is an exponential backoff, with a maximum retry period of 10 minutes.
    fn get_next_retry(&self, retries: i64) -> DateTime<Utc> {
        let retries = retries.min(10) as u32;
        let duration_secs = 2i64.pow(retries).min(600); // clamped to 10 min.

        Utc::now() + Duration::seconds(duration_secs)
    }
}

pub struct AppQueue {
    db_conn: SqlitePool,
    notifier: Notify,
}

/// Central queue interface
///
/// See the crate documentation to see how to use this crate.
impl AppQueue {
    /// Opens or creates a new queue.
    ///
    /// # Errors
    /// This function returns an error if the database cannot be opened, such as when the database is inaccessible or corrupt.
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Arc<Self>> {
        let db_conn = SqlitePool::connect_with(
            SqliteConnectOptions::new()
                .filename(db_path)
                .create_if_missing(true),
        )
        .await
        .context("Opening sqlite database")?;
        let notifier = Notify::new();

        let db = Self { db_conn, notifier };

        db.initialize_db()
            .await
            .context("initializing sqlite datababse")?;

        Ok(Arc::new(db))
    }

    /// Initializes the database if it is uninitialized.
    async fn initialize_db(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.db_conn)
            .await
            .context("Migrating database")?;
        debug!("Database initialized, rescheduling aborted jobs");
        query!("UPDATE jobs SET is_running = 0, retries = retries + 1 WHERE is_running = 1")
            .execute(&self.db_conn)
            .await
            .context("Requeuing aborted jobs")?;
        Ok(())
    }

    /// Runs a single job
    ///
    /// # Errors
    /// This function can fail if there is a database error, or if the format of the event is invalid (due to a missing structure or a deleted event type).
    ///
    /// # Return value
    ///
    /// Returns `false` if there are currently no jobs to run.
    async fn run_job(self: &Arc<Self>) -> Result<bool> {
        let job_info = query!(
            r#"
UPDATE jobs
    SET is_running = 1
    WHERE id IN (
        SELECT id FROM jobs
        WHERE is_running = 0
        AND run_after <= datetime('now')
        ORDER BY run_after ASC
        LIMIT 1)
    RETURNING id, job_data, retries
        "#
        )
        .fetch_optional(&self.db_conn)
        .await
        .context("Loading idle queue entry from database")?;

        let job_info = match job_info {
            Some(job_info) => job_info,
            None => return Ok(false),
        };
        debug!("Fetched job ID: {}", job_info.id);

        let mut de: Box<dyn Job> = ciborium::de::from_reader(job_info.job_data.as_slice())
            .context("Deserializing the job data")?;

        match de.run(Arc::clone(self)).await {
            Ok(()) => {
                debug!("Job {} completed successfully", job_info.id);
                query!("DELETE FROM jobs WHERE id =?", job_info.id)
                    .execute(&self.db_conn)
                    .await?;
            }
            Err(e) => {
                error!("Job {} failed: {:#?}", job_info.id, e);
                if de.is_fatal_error(&e) {
                    error!(
                        "Job {} failed due to fatal error. Aborting retries.",
                        job_info.id
                    );
                    query!("DELETE FROM jobs WHERE id =?", job_info.id)
                        .execute(&self.db_conn)
                        .await?;
                    return Ok(true);
                }
                let next_retry = de.get_next_retry(job_info.retries);
                debug!("Job {} failed. Retrying at {}", job_info.id, next_retry);
                let new_retry_count = job_info.retries + 1;
                let mut job_data = Vec::new();
                ciborium::into_writer(&de, &mut job_data)?;
                query!(
                    "UPDATE jobs SET is_running = 0, run_after =?, retries =?, job_data =? WHERE id =?",
                    next_retry,
                    new_retry_count,
                    job_data,
                    job_info.id,
                )
                .execute(&self.db_conn)
                .await?;
            }
        }

        Ok(true)
    }

    /// Uses the current task to serve as a job runner.
    ///
    /// This future will never resolve, unless an error occurs.
    pub async fn run_job_loop(self: Arc<Self>) -> Result<Never> {
        self.notifier.notify_one();
        info!("Starting job worker.");

        loop {
            self.notifier.notified().await;
            debug!("Received queue notification.");
            while self.run_job().await? {}
            debug!("No more jobs to run for now. Sleeping.")
        }
    }

    /// Spawns a number of worker tasks for running jobs.
    pub fn run_job_workers(self: Arc<Self>, num_workers: usize) {
        for _ in 0..num_workers {
            tokio::spawn(Arc::clone(&self).run_job_loop());
        }
    }

    /// Spawns a default number of worker tasks for running jobs.
    pub fn run_job_workers_default(self: Arc<Self>) {
        self.run_job_workers(num_cpus::get());
    }

    /// Adds a job with a specific opaque ID to the queue.
    ///
    /// This will not do anything if the job is already in the queue.
    pub async fn add_unique_job(&self, id: impl AsRef<str>, job: Box<dyn Job>) -> Result<()> {
        let id = id.as_ref();
        let mut job_data = Vec::new();
        ciborium::into_writer(&job, &mut job_data)?;
        query!(
            "INSERT INTO jobs (unique_job_id, run_after, job_data) VALUES (?,datetime('now'),?)",
            id,
            job_data
        )
        .execute(&self.db_conn)
        .await?;

        self.notifier.notify_one();

        Ok(())
    }

    /// Adds a job to the queue.
    ///
    /// unlike [`add_unique_job`], this will use a random ID.
    pub async fn add_job(&self, job: Box<dyn Job>) -> Result<()> {
        let id = Uuid::new_v4();
        self.add_unique_job(id.to_string(), job).await
    }
}
