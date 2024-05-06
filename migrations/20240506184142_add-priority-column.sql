ALTER TABLE jobs
ADD priority INTEGER NOT NULL DEFAULT 0;
CREATE INDEX jobs_priority ON jobs(priority);
