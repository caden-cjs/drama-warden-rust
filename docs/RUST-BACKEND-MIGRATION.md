# Rust backend status

Drama Warden's application server is Rust. It serves the HTTP API and Vue assets, owns application state and workflow decisions, and provides the optional stdio MCP bridge. The frontend remains a separate Vue application built into the server image.

The former in-process JavaScript application server, business modules, and JavaScript database layer are not part of this release. The JavaScript source retained in the repository is limited to bundled provider adapters and frontend/build tooling. Provider adapters run as isolated child processes through the documented JSON Lines protocol.

## Rust service responsibilities

- Bearer authentication and the panel REST API
- SQLite and MySQL connections, schema migrations, portable export/import, and repository transactions
- Shows, metadata identities, seasons, episodes, storage roots, assets, and episode write claims
- Follow-up planning, queueing, processing, event recovery, and retry policy
- Subtitle queue and worker, runtime settings, scheduled scans, and plugin audit records
- Optional MCP bridge over the same authenticated API

## Runtime migration procedure

For an existing deployment, export its data to a protected portable snapshot, import that snapshot into a separate empty Rust database, and reconcile per-table row counts and representative records. Configure provider authorization separately. Start the Rust service on an unused port with all automatic writes disabled, verify API and provider health, then switch traffic only after the operator accepts the results. Keep the original database, authorization state, and a tested backup available until the new instance is accepted.

The Rust binary supports `--export-database PATH.backup` and `--import-database PATH.backup` without starting the HTTP server. Portable database files contain application data and must be treated as private. See [database configuration](DATABASES.md) and [backup and restore](BACKUP-RESTORE.md).

## Verification

CI runs Rust formatting, workspace tests, and checks; Node adapter contract tests; plugin SDK packaging checks; frontend build; Docker image build and smoke checks; and public-boundary scans over the complete reachable Git history.
