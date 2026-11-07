# Backup and restore

The `drama-warden-data` Docker volume contains the SQLite database when SQLite is selected and local plugin authorization state. It may contain private records and credentials. Restrict access to backups and store them encrypted.

## Backup

Stop the application before copying a SQLite database or its data volume. This avoids copying an inconsistent database. A simple Compose volume copy is:

```sh
backup_dir="backups/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$backup_dir"
docker compose stop drama-warden
docker compose cp --archive drama-warden:/app/data "$backup_dir/data"
docker compose start drama-warden
```

If the application uses MySQL, also create a consistent dump of the configured application database with the MySQL vendor tools. The MySQL data volume does not include `.env` or private `config/config.yaml`; protect those deployment files separately.

The Rust binary can make a portable application-data export without starting the HTTP service. Stop the application first and choose a protected path outside the repository:

```sh
cargo run --locked --manifest-path rust/Cargo.toml -p drama-warden-server -- \
  --export-database /secure/backups/drama-warden.backup
```

## Restore to a new installation

Create a separate Compose project and data volume for the restore. Configure its private environment and provider authorization, then import the portable database snapshot while the application is stopped:

```sh
cargo run --locked --manifest-path rust/Cargo.toml -p drama-warden-server -- \
  --import-database /secure/backups/drama-warden.backup
```

The destination schema must be empty. Import runs transactionally and validates every table's row count. For a volume-level restore, stop the new application and copy the saved `/app/data` contents into its new volume while preserving file ownership and permissions.

Start with follow-up automation and workers disabled. Check `/api/health`, query representative shows and events, inspect storage roots and assets, and verify provider authorization before enabling any scheduled or write-capable operation. Retain the source backup until the restored installation has been accepted.
