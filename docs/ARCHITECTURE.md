# Architecture

## Runtime ownership

The Rust service owns HTTP routing, authentication, database access, show and episode records, follow-up planning, event transitions, write claims, subtitle jobs, scheduling, recovery, and the MCP bridge. The Vue application and CLI use the same authenticated REST API.

SQLite is the default database. MySQL is an optional deployment driver. Both use explicit versioned migrations and the same application records.

## Plugins

External capabilities run as supervised child processes and communicate with the Rust host through versioned JSON Lines messages. The host validates each manifest and capability, sends only declared configuration during initialization, enforces request limits and timeouts, and records metadata-only audit entries.

The bundled Node.js code consists of provider adapters behind this process boundary. It does not serve the application API, own business state, or decide follow-up event outcomes. Third-party plugins can use any language that reads and writes JSON Lines. See [Rust plugin protocol](RUST-PLUGIN-PROTOCOL.md).

Plugin kinds organize providers in the management UI. Runtime calls route by declared capabilities and domains. The Rust core owns workflow order and status semantics; providers normalize external service behavior and return stable errors.

## Follow-up write safety

Follow-up planning resolves metadata identities, canonical seasons and episodes, storage assets, and the candidate share listing before queueing. Queueing creates an event with unique per-episode write claims. Processing rechecks the selected provider file IDs and destination listing, saves only selected items, verifies the destination delta, organizes names and paths, synchronizes assets, and then records a terminal event.

If a provider write may have completed but cannot be confirmed, the Rust service leaves the claim uncertain and blocks automatic retry. Recovery requires an operator to confirm there is no active worker and inspect the complete destination listing.

## Deployment defaults

The Docker image serves the built Vue application from the Rust service. It includes the language runtime needed by bundled process plugins. Optional cloud CLIs are excluded unless explicitly enabled at image build time. Follow-up scheduling, automatic processing, subtitle processing, and MCP are opt-in.

Provider settings belong in a private environment file or a deployment secret store. `config/config.example.yaml` documents non-secret defaults. Never commit database files, provider credentials, authorization volumes, media listings, or operational backups.
