CREATE TABLE IF NOT EXISTS automation_scan_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_type TEXT NOT NULL CHECK (run_type IN ('scheduled', 'manual')),
  started_at TEXT NOT NULL,
  completed_at TEXT NOT NULL,
  shows_checked INTEGER NOT NULL DEFAULT 0 CHECK (shows_checked >= 0),
  storages_checked INTEGER NOT NULL DEFAULT 0 CHECK (storages_checked >= 0),
  candidates_found INTEGER NOT NULL DEFAULT 0 CHECK (candidates_found >= 0),
  eligible_candidates INTEGER NOT NULL DEFAULT 0 CHECK (eligible_candidates >= 0),
  events_queued INTEGER NOT NULL DEFAULT 0 CHECK (events_queued >= 0),
  events_processed INTEGER NOT NULL DEFAULT 0 CHECK (events_processed >= 0),
  events_failed INTEGER NOT NULL DEFAULT 0 CHECK (events_failed >= 0),
  storage_limit_reached INTEGER NOT NULL DEFAULT 0 CHECK (storage_limit_reached IN (0, 1)),
  error_count INTEGER NOT NULL DEFAULT 0 CHECK (error_count >= 0),
  errors_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(errors_json))
);

CREATE INDEX IF NOT EXISTS idx_automation_scan_runs_started
  ON automation_scan_run(started_at DESC, id DESC);
