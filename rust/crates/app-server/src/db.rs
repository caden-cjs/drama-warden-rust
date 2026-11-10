use std::{env, path::Path, time::Duration};

use sqlx::{AnyPool, any::AnyPoolOptions, raw_sql};
use url::Url;

const CURRENT_SCHEMA_VERSION: i64 = 6;
const SQLITE_MIGRATIONS: [&str; 6] = [
    include_str!("../../../migrations/sqlite/0001_core.sql"),
    include_str!("../../../migrations/sqlite/0002_subtitle_jobs.sql"),
    include_str!("../../../migrations/sqlite/0003_application_settings.sql"),
    include_str!("../../../migrations/sqlite/0004_plugin_call_log.sql"),
    include_str!("../../../migrations/sqlite/0005_show_category.sql"),
    include_str!("../../../migrations/sqlite/0006_plugin_configuration.sql"),
];
const MYSQL_MIGRATIONS: [&str; 6] = [
    include_str!("../../../migrations/mysql/0001_core.sql"),
    include_str!("../../../migrations/mysql/0002_subtitle_jobs.sql"),
    include_str!("../../../migrations/mysql/0003_application_settings.sql"),
    include_str!("../../../migrations/mysql/0004_plugin_call_log.sql"),
    include_str!("../../../migrations/mysql/0005_show_category.sql"),
    include_str!("../../../migrations/mysql/0006_plugin_configuration.sql"),
];

#[derive(Clone)]
pub struct Database {
    pub pool: AnyPool,
    pub driver: &'static str,
}

fn mysql_url() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(url) = env::var("DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    let host = env::var("MYSQL_HOST")?;
    let user = env::var("MYSQL_USER")?;
    let password = env::var("MYSQL_PASSWORD").or_else(|_| env::var("MYSQL_PASS"))?;
    let database = env::var("MYSQL_DATABASE").or_else(|_| env::var("MYSQL_DB"))?;
    let port = env::var("MYSQL_PORT")
        .unwrap_or_else(|_| "3306".to_owned())
        .parse::<u16>()?;

    let mut url = Url::parse("mysql://localhost")?;
    url.set_host(Some(&host))?;
    url.set_port(Some(port))
        .map_err(|_| "MYSQL_PORT is invalid")?;
    url.set_username(&user)
        .map_err(|_| "MYSQL_USER is invalid")?;
    url.set_password(Some(&password))
        .map_err(|_| "MYSQL_PASSWORD is invalid")?;
    url.set_path(&format!("/{database}"));
    Ok(url.into())
}

async fn connect_pool(
    url: &str,
    max_connections: u32,
    default_attempts: u32,
) -> Result<AnyPool, sqlx::Error> {
    let attempts = if default_attempts > 1 {
        env::var("MYSQL_CONNECT_ATTEMPTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(default_attempts)
            .clamp(1, 120)
    } else {
        1
    };
    let delay_ms = env::var("MYSQL_CONNECT_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1000)
        .min(30_000);
    let timeout_ms = env::var("MYSQL_CONNECT_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5_000)
        .clamp(100, 60_000);
    let mut last_error = None;
    for attempt in 0..attempts {
        match AnyPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_millis(timeout_ms))
            .connect(url)
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < attempts && delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
    Err(last_error.expect("connection attempts are clamped to at least one"))
}

async fn migrate_sqlite(pool: &AnyPool) -> Result<(), Box<dyn std::error::Error>> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(format!("database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}").into());
    }
    for next_version in version + 1..=CURRENT_SCHEMA_VERSION {
        let script = SQLITE_MIGRATIONS[(next_version - 1) as usize];
        let mut transaction = pool.begin().await?;
        raw_sql(script).execute(&mut *transaction).await?;
        sqlx::query(&format!("PRAGMA user_version = {next_version}"))
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    Ok(())
}

fn split_mysql_statements(script: &str) -> impl Iterator<Item = &str> {
    script
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
}

async fn migrate_mysql(pool: &AnyPool) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?;
    let locked: i64 = sqlx::query_scalar("SELECT GET_LOCK(?, 30)")
        .bind("drama-warden-schema-migration")
        .fetch_one(&mut *connection)
        .await?;
    if locked != 1 {
        return Err("could not acquire MySQL schema migration lock".into());
    }

    let migration_result = async {
        raw_sql(
            "CREATE TABLE IF NOT EXISTS drama_warden_schema_migrations (\
             version INT UNSIGNED NOT NULL PRIMARY KEY, \
             applied_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)\
             ) ENGINE=InnoDB DEFAULT CHARACTER SET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
        ).execute(&mut *connection).await?;
        let version: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM drama_warden_schema_migrations ORDER BY version DESC LIMIT 1",
        ).fetch_optional(&mut *connection).await?;
        let version = version.unwrap_or(0);
        if version > CURRENT_SCHEMA_VERSION {
            return Err(format!("database schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}").into());
        }
        for next_version in version + 1..=CURRENT_SCHEMA_VERSION {
            for statement in split_mysql_statements(MYSQL_MIGRATIONS[(next_version - 1) as usize]) {
                raw_sql(statement).execute(&mut *connection).await?;
            }
            sqlx::query("INSERT INTO drama_warden_schema_migrations (version) VALUES (?)")
                .bind(next_version)
                .execute(&mut *connection).await?;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }.await;

    let release_result = sqlx::query_scalar::<_, Option<i64>>("SELECT RELEASE_LOCK(?)")
        .bind("drama-warden-schema-migration")
        .fetch_one(&mut *connection)
        .await;
    if let Err(error) = release_result {
        if migration_result.is_ok() {
            return Err(error.into());
        }
    }
    migration_result
}

pub async fn connect() -> Result<Database, Box<dyn std::error::Error>> {
    sqlx::any::install_default_drivers();
    let driver = env::var("DB_DRIVER").unwrap_or_else(|_| "sqlite".to_owned());
    let (url, max_connections, attempts) = match driver.as_str() {
        "sqlite" => {
            let path =
                env::var("SQLITE_PATH").unwrap_or_else(|_| "./data/drama-warden.sqlite".to_owned());
            if path != ":memory:" && !path.starts_with("sqlite:") {
                if let Some(parent) = Path::new(&path).parent() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let url = if path.starts_with("sqlite:") {
                path
            } else if path == ":memory:" {
                "sqlite::memory:".to_owned()
            } else {
                format!("sqlite://{path}?mode=rwc")
            };
            (url, 1, 1)
        }
        "mysql" => {
            let connection_limit = env::var("MYSQL_CONNECTION_LIMIT")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(10)
                .clamp(1, 100);
            (mysql_url()?, connection_limit, 30)
        }
        _ => return Err(format!("unsupported DB_DRIVER: {driver}").into()),
    };
    let pool = connect_pool(&url, max_connections, attempts).await?;
    match driver.as_str() {
        "sqlite" => migrate_sqlite(&pool).await?,
        "mysql" => migrate_mysql(&pool).await?,
        _ => unreachable!(),
    }
    Ok(Database {
        pool,
        driver: if driver == "mysql" { "mysql" } else { "sqlite" },
    })
}
