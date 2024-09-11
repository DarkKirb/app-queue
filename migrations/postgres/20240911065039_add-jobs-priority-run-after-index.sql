-- Add migration script here
create index jobs_priority_run_after_running on jobs (priority desc, run_after asc, is_running);