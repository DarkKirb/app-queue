ALTER TABLE jobs
ADD priority BIGINT NOT NULL DEFAULT 0;
CREATE INDEX jobs_priority ON jobs(priority);
