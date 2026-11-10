CREATE TABLE IF NOT EXISTS plugin_configuration (
  plugin_id VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  schema_version INT UNSIGNED NOT NULL DEFAULT 1,
  config_json LONGTEXT NOT NULL,
  secrets_json LONGTEXT NOT NULL,
  revision BIGINT UNSIGNED NOT NULL DEFAULT 1,
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (plugin_id),
  CONSTRAINT chk_plugin_configuration_json CHECK (JSON_VALID(config_json)),
  CONSTRAINT chk_plugin_secrets_json CHECK (JSON_VALID(secrets_json))
) ENGINE=InnoDB DEFAULT CHARACTER SET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS plugin_config_audit (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  plugin_id VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  field_keys_json LONGTEXT NOT NULL,
  secret_field_keys_json LONGTEXT NOT NULL,
  outcome_code VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NULL,
  revision BIGINT UNSIGNED NULL,
  occurred_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  PRIMARY KEY (id),
  KEY idx_plugin_config_audit_recent (occurred_at, id),
  CONSTRAINT chk_plugin_config_audit_fields CHECK (JSON_VALID(field_keys_json)),
  CONSTRAINT chk_plugin_config_audit_secrets CHECK (JSON_VALID(secret_field_keys_json))
) ENGINE=InnoDB DEFAULT CHARACTER SET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
