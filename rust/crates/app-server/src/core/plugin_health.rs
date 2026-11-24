use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{AnyPool, Row};

const QUARK_AUTH_SETTING_KEY: &str = "plugin_auth_quark";
const NOTIFICATION_RETRY_SECONDS: i64 = 15 * 60;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarkAuthorizationState {
    pub authorized: Option<bool>,
    pub checked_at: Option<String>,
    pub last_notified_authorized: Option<bool>,
    pub last_attempted_authorized: Option<bool>,
    pub last_attempt_at_unix: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuarkAuthorizationTransition {
    Expired,
    Restored,
}

pub async fn load_quark_authorization(
    pool: &AnyPool,
) -> Result<QuarkAuthorizationState, sqlx::Error> {
    let row = sqlx::query(
        "SELECT CAST(value_json AS CHAR) AS value_json FROM application_setting WHERE setting_key=?",
    )
    .bind(QUARK_AUTH_SETTING_KEY)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(QuarkAuthorizationState::default());
    };
    let value = row.try_get::<String, _>("value_json").or_else(|_| {
        row.try_get::<Vec<u8>, _>("value_json").and_then(|bytes| {
            String::from_utf8(bytes).map_err(|error| sqlx::Error::Decode(Box::new(error)))
        })
    })?;
    Ok(serde_json::from_str(&value).unwrap_or_default())
}

pub async fn save_quark_authorization(
    pool: &AnyPool,
    mysql: bool,
    state: &QuarkAuthorizationState,
) -> Result<(), sqlx::Error> {
    let value = json!({
        "authorized": state.authorized,
        "checkedAt": state.checked_at,
        "lastNotifiedAuthorized": state.last_notified_authorized,
        "lastAttemptedAuthorized": state.last_attempted_authorized,
        "lastAttemptAtUnix": state.last_attempt_at_unix,
    })
    .to_string();
    let query = if mysql {
        "INSERT INTO application_setting (setting_key,value_json) VALUES (?,?) ON DUPLICATE KEY UPDATE value_json=VALUES(value_json),updated_at=CURRENT_TIMESTAMP(3)"
    } else {
        "INSERT INTO application_setting (setting_key,value_json) VALUES (?,?) ON CONFLICT(setting_key) DO UPDATE SET value_json=excluded.value_json,updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')"
    };
    sqlx::query(query)
        .bind(QUARK_AUTH_SETTING_KEY)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

pub fn authorization_transition(
    previous: Option<bool>,
    authorized: bool,
) -> Option<QuarkAuthorizationTransition> {
    match (previous, authorized) {
        (None, false) | (Some(true), false) => Some(QuarkAuthorizationTransition::Expired),
        (Some(false), true) => Some(QuarkAuthorizationTransition::Restored),
        _ => None,
    }
}

pub fn should_notify(state: &QuarkAuthorizationState, now_unix: i64) -> bool {
    let Some(authorized) = state.authorized else {
        return false;
    };
    if state.last_notified_authorized == Some(authorized) {
        return false;
    }
    if state.last_attempted_authorized != Some(authorized) {
        return true;
    }
    state
        .last_attempt_at_unix
        .is_none_or(|last| now_unix.saturating_sub(last) >= NOTIFICATION_RETRY_SECONDS)
}
