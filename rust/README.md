# Rust backend workspace

This workspace contains the Drama Warden application service and its language-neutral plugin protocol. The Rust service owns HTTP APIs, persistence, follow-up decisions, scheduling, and workflow state. Provider adapters can run as supervised child processes through `crates/plugin-protocol`.

Build with Rust 1.88 or newer:

```sh
cargo check --workspace
cargo run -p drama-warden-server
```

The server requires `API_ACCESS_TOKEN`. SQLite is the default and uses `SQLITE_PATH`; MySQL uses a SQLx `DATABASE_URL`.
