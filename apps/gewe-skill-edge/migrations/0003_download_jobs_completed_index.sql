-- Memory sync pages completed attachments by completion time.
CREATE INDEX IF NOT EXISTS idx_download_jobs_status_completed ON download_jobs(status, completed_at, id);
