CREATE TABLE IF NOT EXISTS automation_scan_run (
  id BIGINT NOT NULL AUTO_INCREMENT,
  run_type VARCHAR(16) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  started_at VARCHAR(32) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  completed_at VARCHAR(32) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  shows_checked BIGINT NOT NULL DEFAULT 0,
  storages_checked BIGINT NOT NULL DEFAULT 0,
  candidates_found BIGINT NOT NULL DEFAULT 0,
  eligible_candidates BIGINT NOT NULL DEFAULT 0,
  events_queued BIGINT NOT NULL DEFAULT 0,
  events_processed BIGINT NOT NULL DEFAULT 0,
  events_failed BIGINT NOT NULL DEFAULT 0,
  storage_limit_reached TINYINT(1) NOT NULL DEFAULT 0,
  error_count BIGINT NOT NULL DEFAULT 0,
  errors_json JSON NOT NULL,
  PRIMARY KEY (id),
  KEY idx_automation_scan_runs_started (started_at, id),
  CONSTRAINT chk_automation_scan_run_type CHECK (run_type IN ('scheduled', 'manual')),
  CONSTRAINT chk_automation_scan_run_counts CHECK (
    shows_checked >= 0 AND storages_checked >= 0 AND candidates_found >= 0
    AND eligible_candidates >= 0 AND events_queued >= 0 AND events_processed >= 0
    AND events_failed >= 0 AND error_count >= 0
  ),
  CONSTRAINT chk_automation_scan_run_limit CHECK (storage_limit_reached IN (0, 1))
) ENGINE=InnoDB DEFAULT CHARACTER SET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
