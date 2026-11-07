# Database configuration and transfer

Drama Warden uses SQLite by default. To use MySQL, select `database.driver: mysql` in a private `config/config.yaml` or set the corresponding deployment environment values. The bundled MySQL service is optional and is started with the `mysql` Compose profile.

Both drivers apply versioned migrations during startup. The database stores application records, runtime settings, plugin enable/order state, events, write claims, subtitle jobs, and metadata-only plugin call history. Provider credentials remain in private deployment configuration and authorization volumes.

## Portable export and import

The Rust server binary can export or import a `.backup` file without starting the HTTP service. Stop the application during transfer and keep the target database empty. The commands use the same database configuration as the service.

```sh
cargo run --locked --manifest-path rust/Cargo.toml -p drama-warden-server -- \
  --export-database backups/pre-migration.backup

cargo run --locked --manifest-path rust/Cargo.toml -p drama-warden-server -- \
  --import-database backups/pre-migration.backup
```

The exporter refuses to overwrite an existing file and creates it with owner-only permissions. Import validates the versioned schema, requires an empty target, writes tables in dependency order inside a transaction, and verifies each table's row count before commit. Keep exports outside Git and protect them like database backups because event history can contain resource references.

Use a separate database or SQLite path when transferring between installations. Do not point the importer at a populated production database. After import, start the service with automation disabled and reconcile shows, storage roots, episodes, assets, events, settings, and provider health before accepting it.

## MySQL notes

The Compose MySQL service uses MySQL 8.4. Production deployments may use an external MySQL service by setting the host, port, database, user, and password in the deployment environment. Back up MySQL with the database vendor's dump tool while following its consistency guidance. Keep the dump and deployment credentials private.
