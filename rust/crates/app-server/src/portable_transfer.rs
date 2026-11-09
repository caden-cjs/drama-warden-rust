use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use serde_json::Value;
use sqlx::{Any, Arguments, Row, any::AnyArguments};
use sqlx::{Column, ValueRef, any::AnyTypeInfoKind};

const TABLES: &[&str] = &[
    "shows",
    "show_metadata_identity",
    "show_season",
    "show_episode",
    "show_episode_metadata",
    "show_storage",
    "show_episode_asset",
    "show_episode_storage",
    "show_episode_write_claim",
    "update_event",
    "subtitle_job",
    "application_setting",
    "plugin_call_log",
    "plugin_configuration",
    "plugin_config_audit",
];

const VERSION_3_TABLES: &[&str] = &[
    "shows",
    "show_metadata_identity",
    "show_season",
    "show_episode",
    "show_episode_metadata",
    "show_storage",
    "show_episode_asset",
    "show_episode_storage",
    "show_episode_write_claim",
    "update_event",
    "subtitle_job",
    "application_setting",
];

const VERSION_4_TABLES: &[&str] = &[
    "shows",
    "show_metadata_identity",
    "show_season",
    "show_episode",
    "show_episode_metadata",
    "show_storage",
    "show_episode_asset",
    "show_episode_storage",
    "show_episode_write_claim",
    "update_event",
    "subtitle_job",
    "application_setting",
    "plugin_call_log",
];

const VERSION_5_TABLES: &[&str] = &[
    "shows",
    "show_metadata_identity",
    "show_season",
    "show_episode",
    "show_episode_metadata",
    "show_storage",
    "show_episode_asset",
    "show_episode_storage",
    "show_episode_write_claim",
    "update_event",
    "subtitle_job",
    "application_setting",
    "plugin_call_log",
];

const MAX_IMPORT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const BINARY_MARKER: &str = "$dramaWardenBinaryV1";
const PRIMARY_KEYS: &[(&str, &str)] = &[
    ("shows", "id"),
    ("show_metadata_identity", "id"),
    ("show_season", "id"),
    ("show_episode", "id"),
    ("show_episode_metadata", "id"),
    ("show_storage", "id"),
    ("show_episode_asset", "id"),
    ("show_episode_storage", "id"),
    ("show_episode_write_claim", "id"),
    ("update_event", "id"),
    ("subtitle_job", "id"),
    ("application_setting", "setting_key"),
    ("plugin_call_log", "id"),
    ("plugin_configuration", "plugin_id"),
    ("plugin_config_audit", "id"),
];

struct TableExport<'a> {
    columns: Vec<&'a str>,
    rows: &'a [Value],
    row_count: usize,
}

enum ImportValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

fn safe_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn parse_table<'a>(table: &'a str, value: &'a Value) -> Result<TableExport<'a>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("invalid export table {table}"))?;
    let columns = object
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("invalid export columns for {table}"))?
        .iter()
        .map(|column| {
            column
                .as_str()
                .filter(|name| safe_identifier(name))
                .ok_or_else(|| format!("invalid export column in {table}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let unique_columns = columns
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if columns.is_empty() || unique_columns.len() != columns.len() {
        return Err(format!("invalid export columns for {table}"));
    }
    let rows = object
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("invalid export rows for {table}"))?;
    let row_count = object
        .get("rowCount")
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| format!("invalid export row count for {table}"))?;
    if row_count != rows.len() {
        return Err(format!("export row count mismatch for {table}"));
    }
    for row in rows {
        let values = row
            .as_object()
            .ok_or_else(|| format!("invalid export row for {table}"))?;
        if values.len() != columns.len()
            || columns.iter().any(|column| !values.contains_key(*column))
        {
            return Err(format!("export row columns mismatch for {table}"));
        }
    }
    Ok(TableExport {
        columns,
        rows,
        row_count,
    })
}

fn validate_snapshot(snapshot: &Value) -> Result<(i64, HashMap<&str, TableExport<'_>>), String> {
    let object = snapshot
        .as_object()
        .ok_or_else(|| "invalid Drama Warden database export".to_owned())?;
    let version = object
        .get("schemaVersion")
        .and_then(Value::as_i64)
        .filter(|version| [3, 4, 5, 6].contains(version))
        .ok_or_else(|| "unsupported Drama Warden database export schema".to_owned())?;
    if object.get("format").and_then(Value::as_str) != Some("drama-warden-database-export")
        || object.get("formatVersion").and_then(Value::as_i64) != Some(1)
        || !matches!(
            object.get("sourceDriver").and_then(Value::as_str),
            Some("sqlite" | "mysql")
        )
    {
        return Err("invalid Drama Warden database export".to_owned());
    }
    let tables_value = object
        .get("tables")
        .and_then(Value::as_object)
        .ok_or_else(|| "database export is missing its table records".to_owned())?;
    let expected = match version {
        3 => VERSION_3_TABLES,
        4 => VERSION_4_TABLES,
        5 => VERSION_5_TABLES,
        _ => TABLES,
    };
    if tables_value.len() != expected.len()
        || expected
            .iter()
            .any(|table| !tables_value.contains_key(*table))
    {
        return Err("database export does not contain the complete application schema".to_owned());
    }
    let mut tables = HashMap::new();
    for table in expected {
        tables.insert(*table, parse_table(table, &tables_value[*table])?);
    }
    Ok((version, tables))
}

async fn table_columns<'e, E>(executor: E, driver: &str, table: &str) -> Result<Vec<String>, String>
where
    E: sqlx::Executor<'e, Database = Any>,
{
    let sql = if driver == "sqlite" {
        format!("PRAGMA table_info('{table}')")
    } else {
        format!("SHOW COLUMNS FROM `{table}`")
    };
    let rows = sqlx::query(&sql)
        .fetch_all(executor)
        .await
        .map_err(|_| format!("could not inspect target table {table}"))?;
    let key = if driver == "sqlite" { "name" } else { "Field" };
    let columns = rows
        .iter()
        .map(|row| {
            row.try_get::<String, _>(key)
                .map_err(|_| format!("could not inspect target table {table}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() {
        return Err(format!("required target table is missing: {table}"));
    }
    Ok(columns)
}

fn is_json_column(table: &str, column: &str) -> bool {
    matches!(
        (table, column),
        ("shows", "aliases_json")
            | ("show_episode_metadata", "metadata_json")
            | ("update_event", "payload_json")
            | ("subtitle_job", "languages_json")
            | ("application_setting", "value_json")
    )
}

fn is_timestamp_column(table: &str, column: &str) -> bool {
    match table {
        "shows" => matches!(column, "created_at" | "updated_at"),
        "show_metadata_identity" => column == "linked_at",
        "show_season" => matches!(column, "metadata_synced_at" | "created_at" | "updated_at"),
        "show_episode" => matches!(column, "created_at" | "updated_at"),
        "show_episode_metadata" => column == "synced_at",
        "show_storage" => matches!(column, "created_at" | "updated_at"),
        "show_episode_asset" => matches!(column, "first_seen_at" | "last_seen_at"),
        "show_episode_storage" => column == "scanned_at",
        "show_episode_write_claim" => matches!(column, "claimed_at" | "updated_at"),
        "update_event" => matches!(
            column,
            "started_at" | "completed_at" | "created_at" | "updated_at"
        ),
        "subtitle_job" => matches!(
            column,
            "next_attempt_at"
                | "locked_until"
                | "write_started_at"
                | "created_at"
                | "updated_at"
                | "completed_at"
        ),
        "application_setting" => column == "updated_at",
        "plugin_call_log" => column == "called_at",
        _ => false,
    }
}

fn mysql_tinyint_column(table: &str, column: &str) -> bool {
    matches!(
        (table, column),
        ("shows", "finished")
            | ("show_storage", "enabled")
            | ("show_episode_asset", "is_present")
            | ("plugin_call_log", "ok")
    )
}

fn mysql_timestamp(value: &str) -> String {
    if value.len() >= 20 && value.as_bytes().get(10) == Some(&b'T') && value.ends_with('Z') {
        value.replace('T', " ").trim_end_matches('Z').to_owned()
    } else {
        value.to_owned()
    }
}

fn portable_timestamp(value: &str) -> String {
    if value.len() < 19 {
        return value.to_owned();
    }
    let bytes = value.as_bytes();
    if bytes.get(10) != Some(&b'T') && bytes.get(10) != Some(&b' ') {
        return value.to_owned();
    }
    let fraction = value
        .get(19..)
        .unwrap_or_default()
        .trim_end_matches('Z')
        .trim_start_matches('.');
    let fraction = if fraction.is_empty() {
        "000".to_owned()
    } else {
        format!("{:0<3}", fraction).chars().take(3).collect()
    };
    format!("{}T{}.{}Z", &value[..10], &value[11..19], fraction)
}

pub(crate) fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let bits = (a << 16) | (b << 8) | c;
        output.push(ALPHABET[((bits >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((bits >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((bits >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(bits & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn export_cell(
    row: &sqlx::any::AnyRow,
    column: usize,
    table: &str,
    name: &str,
) -> Result<Value, String> {
    let raw = row
        .try_get_raw(column)
        .map_err(|_| format!("could not read column from table {table}"))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let value = match raw.type_info().kind() {
        AnyTypeInfoKind::Null => Value::Null,
        AnyTypeInfoKind::Bool => Value::Bool(
            row.try_get(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::SmallInt => Value::from(
            row.try_get::<i16, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::Integer => Value::from(
            row.try_get::<i32, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::BigInt => Value::from(
            row.try_get::<i64, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::Real => Value::from(
            row.try_get::<f32, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::Double => Value::from(
            row.try_get::<f64, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?,
        ),
        AnyTypeInfoKind::Text => {
            let text = row
                .try_get::<String, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?;
            if is_timestamp_column(table, name) {
                Value::String(portable_timestamp(&text))
            } else {
                Value::String(text)
            }
        }
        AnyTypeInfoKind::Blob => {
            let bytes = row
                .try_get::<Vec<u8>, _>(column)
                .map_err(|_| format!("could not read column from table {table}"))?;
            if is_json_column(table, name) {
                Value::String(
                    String::from_utf8(bytes)
                        .map_err(|_| format!("invalid UTF-8 JSON value in {table}.{name}"))?,
                )
            } else {
                serde_json::json!({BINARY_MARKER: encode_base64(&bytes)})
            }
        }
    };
    Ok(value)
}

pub async fn export_snapshot(pool: &sqlx::AnyPool, driver: &str) -> Result<Value, String> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "could not start database export snapshot".to_owned())?;
    let mut exported_tables = serde_json::Map::new();
    for table in TABLES {
        let columns = table_columns(&mut *transaction, driver, table).await?;
        let (_, primary_key) = PRIMARY_KEYS
            .iter()
            .find(|(name, _)| name == table)
            .expect("all portable tables have a primary key");
        let select_columns = columns
            .iter()
            .map(|column| {
                if driver == "mysql" && mysql_tinyint_column(table, column) {
                    format!("CAST(`{column}` AS SIGNED) AS `{column}`")
                } else if driver == "mysql"
                    && (is_timestamp_column(table, column) || is_json_column(table, column))
                {
                    format!("CAST(`{column}` AS CHAR) AS `{column}`")
                } else {
                    format!("`{column}`")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!("SELECT {select_columns} FROM `{table}` ORDER BY `{primary_key}`");
        let rows = sqlx::query(&query)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| {
                format!("could not read rows from table {table} ({query}): {error}")
            })?;
        let mut exported_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut object = serde_json::Map::new();
            for (index, column) in row.columns().iter().enumerate() {
                object.insert(
                    column.name().to_owned(),
                    export_cell(row, index, table, column.name())?,
                );
            }
            exported_rows.push(Value::Object(object));
        }
        exported_tables.insert(
            table.to_string(),
            serde_json::json!({
                "columns": columns,
                "rowCount": exported_rows.len(),
                "rows": exported_rows,
            }),
        );
    }
    let exported_at = if driver == "mysql" {
        sqlx::query_scalar::<_, String>(
            "SELECT DATE_FORMAT(UTC_TIMESTAMP(3), '%Y-%m-%dT%H:%i:%s.%fZ')",
        )
        .fetch_one(&mut *transaction)
        .await
    } else {
        sqlx::query_scalar::<_, String>("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
            .fetch_one(&mut *transaction)
            .await
    }
    .map_err(|_| "could not timestamp database export".to_owned())?;
    transaction
        .commit()
        .await
        .map_err(|_| "could not close database export snapshot".to_owned())?;
    Ok(serde_json::json!({
        "format": "drama-warden-database-export",
        "formatVersion": 1,
        "schemaVersion": 6,
        "sourceDriver": driver,
        "exportedAt": portable_timestamp(&exported_at),
        "tables": exported_tables,
    }))
}

pub async fn export_file(
    pool: &sqlx::AnyPool,
    driver: &str,
    path: &Path,
) -> Result<std::collections::BTreeMap<String, usize>, String> {
    let snapshot = export_snapshot(pool, driver).await?;
    let tables = snapshot
        .get("tables")
        .and_then(Value::as_object)
        .ok_or_else(|| "could not build database export".to_owned())?;
    let mut counts = std::collections::BTreeMap::new();
    for (table, section) in tables {
        let count = section
            .get("rowCount")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| format!("could not count exported rows for {table}"))?;
        counts.insert(table.clone(), count);
    }
    let mut bytes = serde_json::to_vec(&snapshot)
        .map_err(|_| "could not serialize database export".to_owned())?;
    bytes.push(b'\n');
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|_| "could not create database export directory".to_owned())?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "could not create private database export file".to_owned())?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err("could not write database export file".to_owned());
    }
    Ok(counts)
}

fn decode_value(
    value: &Value,
    driver: &str,
    table: &str,
    column: &str,
) -> Result<ImportValue, String> {
    if value.is_null() {
        return Ok(ImportValue::Null);
    }
    if is_timestamp_column(table, column) {
        if let Some(text) = value.as_str() {
            let value = if driver == "mysql" {
                mysql_timestamp(text)
            } else {
                text.to_owned()
            };
            return Ok(ImportValue::Text(value));
        }
    }
    match value {
        Value::Bool(value) => Ok(ImportValue::Bool(*value)),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(ImportValue::Integer(value))
            } else if let Some(value) = value.as_u64() {
                i64::try_from(value).map(ImportValue::Integer).map_err(|_| {
                    "database export contains an integer outside the supported range".to_owned()
                })
            } else {
                value
                    .as_f64()
                    .map(ImportValue::Float)
                    .ok_or_else(|| "database export contains an invalid number".to_owned())
            }
        }
        Value::String(value) => {
            if is_json_column(table, column) {
                serde_json::from_str::<Value>(value)
                    .map_err(|_| format!("invalid JSON value in {table}.{column}"))?;
            }
            Ok(ImportValue::Text(value.clone()))
        }
        Value::Object(object) if object.len() == 1 && object.contains_key(BINARY_MARKER) => {
            let encoded = object[BINARY_MARKER]
                .as_str()
                .ok_or_else(|| "database export contains invalid binary data".to_owned())?;
            let bytes = decode_base64(encoded)?;
            Ok(ImportValue::Blob(bytes))
        }
        _ => Err("database export contains an unsupported value".to_owned()),
    }
}

pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if input.len() % 4 != 0 {
        return Err("database export contains invalid binary data".to_owned());
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let chunks = input.as_bytes().chunks_exact(4);
    let chunk_count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let final_chunk = index + 1 == chunk_count;
        if (!final_chunk && chunk.contains(&b'='))
            || (chunk[2] == b'=' && chunk[3] != b'=')
            || (chunk[2] == b'=' && chunk[1] & 0x0f != 0)
            || (chunk[3] == b'=' && chunk[2] != b'=' && chunk[2] & 0x03 != 0)
        {
            return Err("database export contains invalid binary data".to_owned());
        }
        let a = ALPHABET
            .iter()
            .position(|byte| *byte == chunk[0])
            .ok_or_else(|| "database export contains invalid binary data".to_owned())?
            as u32;
        let b = ALPHABET
            .iter()
            .position(|byte| *byte == chunk[1])
            .ok_or_else(|| "database export contains invalid binary data".to_owned())?
            as u32;
        let c = if chunk[2] == b'=' {
            0
        } else {
            ALPHABET
                .iter()
                .position(|byte| *byte == chunk[2])
                .ok_or_else(|| "database export contains invalid binary data".to_owned())?
                as u32
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            ALPHABET
                .iter()
                .position(|byte| *byte == chunk[3])
                .ok_or_else(|| "database export contains invalid binary data".to_owned())?
                as u32
        };
        let bits = (a << 18) | (b << 12) | (c << 6) | d;
        output.push((bits >> 16) as u8);
        if chunk[2] != b'=' {
            output.push((bits >> 8) as u8);
        }
        if chunk[3] != b'=' {
            output.push(bits as u8);
        }
    }
    if encode_base64(&output) != input {
        return Err("database export contains invalid binary data".to_owned());
    }
    Ok(output)
}

async fn table_count<'e, E>(executor: E, table: &str) -> Result<i64, String>
where
    E: sqlx::Executor<'e, Database = Any>,
{
    sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM `{table}`"))
        .fetch_one(executor)
        .await
        .map_err(|_| format!("could not verify target table {table}"))
}

pub async fn import_file(
    pool: &sqlx::AnyPool,
    driver: &str,
    path: &Path,
) -> Result<std::collections::BTreeMap<String, usize>, String> {
    let metadata = fs::metadata(path).map_err(|_| "could not read database export".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_IMPORT_BYTES {
        return Err("database export is not a regular file or exceeds the 2 GiB limit".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("database export permissions must be 0600 or stricter".to_owned());
        }
    }
    let bytes = fs::read(path).map_err(|_| "could not read database export".to_owned())?;
    let snapshot: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "database export is not valid JSON".to_owned())?;
    let (version, exports) = validate_snapshot(&snapshot)?;

    for table in TABLES {
        let columns = table_columns(pool, driver, table).await?;
        if let Some(export) = exports.get(table) {
            let target_set = columns
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            let legacy_show_without_category = *table == "shows"
                && version < 5
                && export.columns.len() + 1 == columns.len()
                && columns
                    .iter()
                    .filter(|column| column.as_str() != "category")
                    .all(|column| export.columns.contains(&column.as_str()));
            if (!legacy_show_without_category && export.columns.len() != columns.len())
                || export
                    .columns
                    .iter()
                    .any(|column| !target_set.contains(column))
            {
                return Err(format!(
                    "database export schema does not match target table {table}"
                ));
            }
        } else if !(version < 4 && *table == "plugin_call_log") {
            return Err(format!("database export is missing required table {table}"));
        }
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "could not start database import transaction".to_owned())?;
    for table in TABLES {
        if table_count(&mut *transaction, table).await? != 0 {
            return Err(format!(
                "target database must be empty; table {table} already contains rows"
            ));
        }
    }

    for table in TABLES {
        let Some(export) = exports.get(table) else {
            continue;
        };
        if export.row_count == 0 {
            continue;
        }
        let columns = &export.columns;
        let column_sql = columns
            .iter()
            .map(|column| format!("`{column}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let values_sql = (0..columns.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!("INSERT INTO `{table}` ({column_sql}) VALUES ({values_sql})");
        for row in export.rows {
            let values = row.as_object().expect("snapshot row validated");
            let mut arguments = AnyArguments::default();
            for column in columns {
                let value = decode_value(&values[*column], driver, table, column)?;
                let result = match value {
                    ImportValue::Null => arguments.add(Option::<String>::None),
                    ImportValue::Bool(value) => arguments.add(value),
                    ImportValue::Integer(value) => arguments.add(value),
                    ImportValue::Float(value) => arguments.add(value),
                    ImportValue::Text(value) => arguments.add(value),
                    ImportValue::Blob(value) => arguments.add(value),
                };
                result.map_err(|_| format!("invalid value in database export table {table}"))?;
            }
            sqlx::query_with::<Any, _>(&query, arguments)
                .execute(&mut *transaction)
                .await
                .map_err(|_| format!("database import failed while inserting table {table}"))?;
        }
    }

    let mut imported = std::collections::BTreeMap::new();
    for table in TABLES {
        let count = table_count(&mut *transaction, table).await?;
        let expected = exports.get(table).map_or(0, |export| export.row_count);
        if usize::try_from(count).ok() != Some(expected) {
            return Err(format!(
                "database import row count verification failed for {table}"
            ));
        }
        imported.insert((*table).to_owned(), expected);
    }
    transaction
        .commit()
        .await
        .map_err(|_| "could not commit database import".to_owned())?;
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::{is_json_column, is_timestamp_column, mysql_tinyint_column};

    #[test]
    fn mysql_native_columns_with_nonportable_types_are_classified() {
        assert!(mysql_tinyint_column("shows", "finished"));
        assert!(mysql_tinyint_column("show_storage", "enabled"));
        assert!(mysql_tinyint_column("show_episode_asset", "is_present"));
        assert!(mysql_tinyint_column("plugin_call_log", "ok"));
        assert!(!mysql_tinyint_column("shows", "name"));
        assert!(is_json_column("shows", "aliases_json"));
        assert!(is_json_column("application_setting", "value_json"));
        assert!(is_timestamp_column("shows", "created_at"));
    }
}
