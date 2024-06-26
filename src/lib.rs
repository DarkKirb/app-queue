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

use std::{
    convert::Infallible,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::{query, sqlite::SqliteConnectOptions, SqlitePool};
use tokio::{sync::Notify, time::timeout};
use tracing::{debug, error, info};
use uuid::Uuid;

/// The Never `!` type. Can’t be created. Indicates that the function will never return.
#[deprecated(since = "0.1.1", note = "Will become private API in 0.2.0")]
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

/// Central queue interface
///
/// See the crate documentation to see how to use this crate.
pub struct AppQueue {
    db_conn: SqlitePool,
    runner_notifier: Notify,
    monitor_notifier: Notify,
    monitor_spawned: AtomicBool,
}

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

        let db = Self {
            db_conn,
            runner_notifier: Notify::new(),
            monitor_notifier: Notify::new(),
            monitor_spawned: AtomicBool::new(false),
        };

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

    /// Wakes up all executor tasks.
    ///
    /// This is useful for when you
    fn wake_up_executor_tasks(&self) {
        self.runner_notifier.notify_waiters();
        self.runner_notifier.notify_one();
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
        let now = Utc::now();
        let job_info = query!(
            r#"
UPDATE jobs
    SET is_running = 1
    WHERE id IN (
        SELECT id FROM jobs
        WHERE is_running = 0
        AND run_after <= ?
        ORDER BY priority DESC, run_after ASC
        LIMIT 1)
    RETURNING id, job_data, retries
        "#,
            now
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
                self.monitor_notifier.notify_one();
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
                    self.monitor_notifier.notify_one();
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
                self.monitor_notifier.notify_one();
            }
        }

        Ok(true)
    }

    #[allow(deprecated)]
    async fn monitor_job(self: Arc<Self>) -> Result<Never> {
        loop {
            let now = Utc::now();

            let query_res = query!(
                "SELECT id FROM jobs
        WHERE is_running = 0
        AND run_after <= ? LIMIT 1",
                now
            )
            .fetch_optional(&self.db_conn)
            .await?;
            if query_res.is_some() {
                self.wake_up_executor_tasks();
            }

            // Sleep until the executor is notified or
            timeout(
                std::time::Duration::from_secs(10),
                self.monitor_notifier.notified(),
            )
            .await
            .ok();
        }
    }

    /// Uses the current task to serve as a job runner.
    ///
    /// This future will never resolve, unless an error occurs.
    #[deprecated(
        since = "0.1.1",
        note = "Use `run_job_workers` or `run_job_workers_default` instead."
    )]
    #[allow(deprecated)]
    pub async fn run_job_loop(self: Arc<Self>) -> Result<Never> {
        // TODO for version 0.2.0: this shouldn’t be in here!
        if !self.monitor_spawned.swap(true, Ordering::Relaxed) {
            let self2 = Arc::clone(&self);
            tokio::spawn(self2.monitor_job());
        }

        self.runner_notifier.notify_one();
        info!("Starting job worker.");

        loop {
            while self.run_job().await? {}
            debug!("No more jobs to run for now. Sleeping.");
            self.runner_notifier.notified().await;
            debug!("Received queue notification.");
        }
    }

    /// Spawns a number of worker tasks for running jobs.
    #[allow(deprecated)]
    pub fn run_job_workers(self: Arc<Self>, num_workers: usize) {
        for _ in 0..num_workers {
            tokio::spawn(Arc::clone(&self).run_job_loop());
        }
    }

    /// Spawns a default number of worker tasks for running jobs.
    pub fn run_job_workers_default(self: Arc<Self>) {
        self.run_job_workers(num_cpus::get());
    }

    async fn schedule_job<J: Job>(&self, job: JobBuilder<J>) -> Result<()> {
        let job_boxed: Box<dyn Job> = Box::new(job.job);
        let mut job_data = Vec::new();
        ciborium::into_writer(&job_boxed, &mut job_data)?;
        query!(
            "INSERT INTO jobs (unique_job_id, run_after, job_data, priority) VALUES (?, ?, ?, ?)",
            job.id,
            job.run_after,
            job_data,
            job.priority
        )
        .execute(&self.db_conn)
        .await?;
        self.monitor_notifier.notify_one();
        Ok(())
    }

    /// Adds a job with a specific opaque ID to the queue.
    ///
    /// This will not do anything if the job is already in the queue.
    pub async fn add_unique_job(&self, id: impl AsRef<str>, job: Box<dyn Job>) -> Result<()> {
        // TODO: deduplicate with the above
        let id = id.as_ref();
        let mut job_data = Vec::new();
        ciborium::into_writer(&job, &mut job_data)?;
        let now = Utc::now();
        query!(
            "INSERT INTO jobs (unique_job_id, run_after, job_data) VALUES (?,?,?)",
            id,
            now,
            job_data
        )
        .execute(&self.db_conn)
        .await?;

        self.monitor_notifier.notify_one();

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

/// A builder-style interface for scheduling jobs.
#[derive(Clone, Debug)]
pub struct JobBuilder<J: Job> {
    job: J,
    id: String,
    run_after: DateTime<Utc>,
    priority: i64,
}

impl<J: Job> JobBuilder<J> {
    /// Creates a new job builder.
    pub fn new(job: J) -> Self {
        Self {
            job,
            id: Uuid::new_v4().to_string(),
            run_after: Utc::now(),
            priority: 0,
        }
    }

    /// Sets the ID of the job.
    ///
    /// Set this to a deterministic value to ensure that a job is only scheduled once.
    pub fn id(mut self, id: impl ToString) -> Self {
        self.id = id.to_string();
        self
    }

    /// Sets the time after which the job is allowed to run.
    ///
    /// This doesn’t guarantee execution at that time.
    pub fn run_after(mut self, run_after: DateTime<Utc>) -> Self {
        self.run_after = run_after;
        self
    }

    /// Schedules the job to the queue.
    pub async fn schedule(self, app_queue: &AppQueue) -> Result<()> {
        app_queue.schedule_job(self).await
    }

    /// Changes the priority of the job.
    ///
    /// Jobs that are (over)due will run in order of priority, and then in order of how long the job has been due.
    ///
    /// A higher number indicates a higher priority. The default priority is 0.
    pub fn priority(mut self, prio: i64) -> Self {
        self.priority = prio;
        self
    }
}
