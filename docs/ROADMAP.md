# Roadmap

## Core service

- Rust HTTP API, authentication, SQLite and MySQL migrations
- Shows, metadata identities, seasons, episodes, storage assets, and plugin settings
- Follow-up preview, queue, processing, write claims, event recovery, and retry controls
- Subtitle queue and worker, runtime settings, scheduling, portable database transfer, and optional MCP bridge

## Provider ecosystem

- Versioned JSON Lines protocol for supervised language-independent plugins
- Bundled metadata, resource search, cloud storage, local storage, subtitle, indexer, and notification adapters
- Public plugin SDK and reference plugin examples
- Provider health and metadata-only capability audit records

## Ongoing work

- Expand compatibility coverage for additional metadata providers and storage vendors.
- Improve operator-facing import/export and recovery workflows as deployments gain experience.
- Keep third-party CLI versions, licenses, and service requirements documented and reviewed.
- Continue API compatibility and plugin protocol tests as new capabilities are added.

Follow-up automation and automatic media writes remain opt-in. New provider integrations must not make ambiguous writes retry automatically or bypass the Rust workflow state machine.
