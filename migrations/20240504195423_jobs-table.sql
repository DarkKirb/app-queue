-- Add migration script here
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY NOT NULL,
    unique_job_id TEXT NOT NULL,
    is_running INTEGER NOT NULL DEFAULT 0,
    retries INTEGER NOT NULL DEFAULT 0,
    run_after DATETIME NOT NULL,
    job_data BLOB NOT NULL
);
CREATE INDEX jobs_running ON jobs (is_running);
CREATE INDEX jobs_run_after ON jobs (run_after);
CREATE UNIQUE INDEX jobs_unique_id ON jobs(unique_job_id);
