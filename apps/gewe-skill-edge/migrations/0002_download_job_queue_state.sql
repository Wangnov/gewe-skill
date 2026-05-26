ALTER TABLE download_jobs ADD COLUMN locked_until TEXT;
ALTER TABLE download_jobs ADD COLUMN next_attempt_at TEXT;
ALTER TABLE download_jobs ADD COLUMN terminal_at TEXT;

CREATE INDEX IF NOT EXISTS idx_download_jobs_status_next_attempt ON download_jobs(status, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_download_jobs_locked_until ON download_jobs(locked_until);
