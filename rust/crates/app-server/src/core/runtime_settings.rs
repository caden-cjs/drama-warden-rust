use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::{AnyPool, Row};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettings {
    pub followup_automation_enabled: bool,
    pub followup_scan_interval_minutes: i64,
    pub followup_auto_process: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettingsResponse {
    pub settings: RuntimeSettings,
    pub defaults: RuntimeSettings,
    pub source: &'static str,
}

#[derive(Debug)]
pub enum SettingsError {
    Invalid(&'static str),
    InvalidStored,
    Database(sqlx::Error),
}

impl IntoResponse for SettingsError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Invalid(code) => (StatusCode::BAD_REQUEST, code, "invalid runtime settings"),
            Self::InvalidStored => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_settings_invalid",
                "stored runtime settings are invalid",
            ),
            Self::Database(error) => {
                tracing::error!(error = %error, "runtime settings database operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "request failed",
                )
            }
        };
        (status, Json(json!({"error":message,"code":code}))).into_response()
    }
}

impl RuntimeSettings {
    fn from_map(value: &Map<String, Value>) -> Result<Self, SettingsError> {
        let automation = value
            .get("followupAutomationEnabled")
            .and_then(Value::as_bool)
            .ok_or(SettingsError::Invalid("invalid_runtime_settings"))?;
        let interval = value
            .get("followupScanIntervalMinutes")
            .and_then(integer_value)
            .filter(|value| (5..=1440).contains(value))
            .ok_or(SettingsError::Invalid("invalid_runtime_settings"))?;
        let auto_process = value
            .get("followupAutoProcess")
            .and_then(Value::as_bool)
            .ok_or(SettingsError::Invalid("invalid_runtime_settings"))?;
        if value.len() != 3 {
            return Err(SettingsError::Invalid("invalid_runtime_settings"));
        }
        Ok(Self {
            followup_automation_enabled: automation,
            followup_scan_interval_minutes: interval,
            followup_auto_process: auto_process,
        })
    }

    fn to_value(&self) -> Value {
        json!({
            "followupAutomationEnabled":self.followup_automation_enabled,
            "followupScanIntervalMinutes":self.followup_scan_interval_minutes,
            "followupAutoProcess":self.followup_auto_process,
        })
    }
}

fn integer_value(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && *number >= i64::MIN as f64
                    && *number <= i64::MAX as f64
            })
            .map(|number| number as i64)
    })
}

fn env_bool(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|value| {
        ["1", "true", "yes", "on"]
            .iter()
            .any(|valid| value.trim().eq_ignore_ascii_case(valid))
    })
}

pub fn defaults() -> Result<RuntimeSettings, SettingsError> {
    let interval = match std::env::var("FOLLOWUP_SCAN_INTERVAL_MINUTES") {
        Ok(value) if value.trim().is_empty() => 30,
        Ok(value) => value
            .parse::<f64>()
            .ok()
            .filter(|value| {
                value.is_finite() && value.fract() == 0.0 && (5.0..=1440.0).contains(value)
            })
            .map(|value| value as i64)
            .ok_or(SettingsError::Invalid("invalid_runtime_defaults"))?,
        Err(_) => 30,
    };
    Ok(RuntimeSettings {
        followup_automation_enabled: env_bool("FOLLOWUP_AUTOMATION_ENABLED"),
        followup_scan_interval_minutes: interval,
        followup_auto_process: env_bool("FOLLOWUP_AUTO_PROCESS"),
    })
}

fn parse_setting(value: &str) -> Result<RuntimeSettings, SettingsError> {
    let parsed: Value = serde_json::from_str(value).map_err(|_| SettingsError::InvalidStored)?;
    let object = parsed.as_object().ok_or(SettingsError::InvalidStored)?;
    RuntimeSettings::from_map(object).map_err(|_| SettingsError::InvalidStored)
}

async fn read_stored(
    pool: &AnyPool,
    mysql: bool,
    lock: bool,
) -> Result<Option<RuntimeSettings>, SettingsError> {
    let query = format!(
        "SELECT CAST(value_json AS CHAR) AS value_json FROM application_setting WHERE setting_key='runtime'{}",
        if mysql && lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&query)
        .fetch_optional(pool)
        .await
        .map_err(SettingsError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let value = row
        .try_get::<String, _>("value_json")
        .or_else(|_| {
            row.try_get::<Vec<u8>, _>("value_json").and_then(|bytes| {
                String::from_utf8(bytes).map_err(|error| sqlx::Error::Decode(Box::new(error)))
            })
        })
        .map_err(SettingsError::Database)?;
    parse_setting(&value).map(Some)
}

pub async fn get(pool: &AnyPool, mysql: bool) -> Result<RuntimeSettingsResponse, SettingsError> {
    let defaults = defaults()?;
    let stored = read_stored(pool, mysql, false).await?;
    Ok(RuntimeSettingsResponse {
        settings: stored.clone().unwrap_or_else(|| defaults.clone()),
        defaults,
        source: if stored.is_some() {
            "database"
        } else {
            "environment"
        },
    })
}

pub async fn update(
    pool: &AnyPool,
    input: Value,
    mysql: bool,
) -> Result<RuntimeSettingsResponse, SettingsError> {
    let patch = input
        .as_object()
        .ok_or(SettingsError::Invalid("runtime_settings_object_required"))?;
    if patch.is_empty() {
        return Err(SettingsError::Invalid("runtime_settings_patch_required"));
    }
    for key in patch.keys() {
        if ![
            "followupAutomationEnabled",
            "followupScanIntervalMinutes",
            "followupAutoProcess",
        ]
        .contains(&key.as_str())
        {
            return Err(SettingsError::Invalid("unsupported_runtime_setting"));
        }
    }
    let defaults = defaults()?;
    let mut tx = pool.begin().await.map_err(SettingsError::Database)?;
    let query = format!(
        "SELECT CAST(value_json AS CHAR) AS value_json FROM application_setting WHERE setting_key='runtime'{}",
        if mysql { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&query)
        .fetch_optional(&mut *tx)
        .await
        .map_err(SettingsError::Database)?;
    let mut merged = row
        .as_ref()
        .map(|row| {
            let value = row
                .try_get::<String, _>("value_json")
                .or_else(|_| {
                    row.try_get::<Vec<u8>, _>("value_json").and_then(|bytes| {
                        String::from_utf8(bytes)
                            .map_err(|error| sqlx::Error::Decode(Box::new(error)))
                    })
                })
                .map_err(SettingsError::Database)?;
            let parsed: Value =
                serde_json::from_str(&value).map_err(|_| SettingsError::InvalidStored)?;
            parsed
                .as_object()
                .cloned()
                .ok_or(SettingsError::InvalidStored)
        })
        .transpose()?
        .unwrap_or_else(|| {
            serde_json::to_value(&defaults)
                .unwrap_or(Value::Null)
                .as_object()
                .cloned()
                .unwrap_or_default()
        });
    for (key, value) in patch {
        merged.insert(key.clone(), value.clone());
    }
    let settings = RuntimeSettings::from_map(&merged)?;
    let query = if mysql {
        "INSERT INTO application_setting (setting_key,value_json) VALUES ('runtime',?) ON DUPLICATE KEY UPDATE value_json=VALUES(value_json),updated_at=CURRENT_TIMESTAMP(3)"
    } else {
        "INSERT INTO application_setting (setting_key,value_json) VALUES ('runtime',?) ON CONFLICT(setting_key) DO UPDATE SET value_json=excluded.value_json,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    sqlx::query(query)
        .bind(settings.to_value().to_string())
        .execute(&mut *tx)
        .await
        .map_err(SettingsError::Database)?;
    tx.commit().await.map_err(SettingsError::Database)?;
    Ok(RuntimeSettingsResponse {
        settings,
        defaults,
        source: "database",
    })
}

pub async fn reset(pool: &AnyPool) -> Result<RuntimeSettingsResponse, SettingsError> {
    let defaults = defaults()?;
    sqlx::query("DELETE FROM application_setting WHERE setting_key='runtime'")
        .execute(pool)
        .await
        .map_err(SettingsError::Database)?;
    Ok(RuntimeSettingsResponse {
        settings: defaults.clone(),
        defaults,
        source: "environment",
    })
}
