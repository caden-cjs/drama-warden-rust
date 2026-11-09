CREATE TABLE IF NOT EXISTS plugin_configuration (
  plugin_id TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version > 0),
  config_json TEXT NOT NULL CHECK (json_valid(config_json)),
  secrets_json TEXT NOT NULL CHECK (json_valid(secrets_json)),
  revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS plugin_config_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  plugin_id TEXT NOT NULL,
  field_keys_json TEXT NOT NULL CHECK (json_valid(field_keys_json)),
  secret_field_keys_json TEXT NOT NULL CHECK (json_valid(secret_field_keys_json)),
  outcome_code TEXT,
  revision INTEGER,
  occurred_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_plugin_config_audit_recent
  ON plugin_config_audit (occurred_at DESC, id DESC);
