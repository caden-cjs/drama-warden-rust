# Drama Warden

Drama Warden is a self-hosted tracker for TV, anime, movies, and variety shows. The Rust service manages shows, seasons, episodes, storage assets, follow-up events, and the web API. The Vue panel uses that API. External metadata, resource search, storage, subtitles, indexing, and notifications connect through versioned plugins.

The project is under active development. Review [the API](docs/API.md), [provider setup](docs/PROVIDERS.md), and [third-party notices](THIRD-PARTY-NOTICES.md) before enabling provider integrations.

## Features

- Track seasons, episodes, multiple media assets, missing episodes, and conflicts.
- Preview a resource share, select matching episodes, and queue a follow-up event.
- Verify selected files and destination changes before marking a write complete. Ambiguous writes require operator review and are never retried automatically.
- Add metadata, resource, storage, subtitle, indexer, notification, and workflow plugins through a language-neutral JSON Lines process protocol.
- Use SQLite by default or configure MySQL.
- Manage the service through the Vue panel, REST API, CLI, or optional stdio MCP bridge.
- Keep scheduled scanning and automatic media writes disabled until explicitly enabled.

## Run with Docker Compose

Docker Compose v2 is the recommended deployment. Generate an API token, place it in `.env`, and start the service:

```sh
cp .env.example .env
cp config/config.example.yaml config/config.yaml
openssl rand -hex 32
```

Set the generated value as `API_ACCESS_TOKEN`, then run:

```sh
docker compose up --build -d
```

Open <http://localhost:18889>. The service binds the host port from `HOST_PORT` (18889 by default) and listens on port 8080 inside the container. `GET /api/health` is public; all other API routes require the configured bearer token. The panel asks for the token in its connection settings.

The named `drama-warden-data` volume stores the SQLite database and plugin authorization state. Back up this volume before upgrades and follow [the backup and restore guide](docs/BACKUP-RESTORE.md).

## Local development

Install Rust 1.88 or later, Node.js 22.15 or later, and npm. Node is used for the frontend toolchain and bundled Node process plugins; the application HTTP server is Rust.

```sh
npm ci
npm run build
cargo test --locked --manifest-path rust/Cargo.toml
cargo run --locked --manifest-path rust/Cargo.toml -p drama-warden-server
```

Set `API_ACCESS_TOKEN` and any provider configuration in the process environment or a private `.env` file. The Rust service loads `config/config.yaml` when present and otherwise uses `config/config.example.yaml`.

The CLI calls the same authenticated API as the panel:

```sh
npm run cli -- shows list
npm run cli -- metadata search --query "Show title"
npm run cli -- shows create --name "Show title"
npm run cli -- shows link-metadata 1 --provider tvmaze --external-id 12345
npm run cli -- storage scan --show-id 1 --storage-id 1
```

Pass share URLs to CLI commands through standard input. Queueing a share does not write media; processing a queued event can write through the selected storage provider.

## Plugin model

The Rust host starts plugins as supervised child processes and speaks the versioned protocol in [Rust plugin protocol](docs/RUST-PLUGIN-PROTOCOL.md). Plugins declare their ID, kind, capabilities, configuration fields, and lifecycle hooks. The host sends only manifest-declared settings during initialization and records metadata-only capability audit entries.

The repository includes provider adapters implemented in Node.js. They run behind the same process protocol as plugins written in Rust, Python, or another language. Optional third-party CLIs are excluded from the default image; see [provider setup](docs/PROVIDERS.md) and `INCLUDE_QUARK_CLI` / `INCLUDE_115_CLI` in `.env.example`.

## Safety and service scope

This project is intended for personal learning and self-hosting. Some providers rely on third-party interfaces or command-line tools whose behavior and terms may change. Operators are responsible for provider accounts, applicable terms, media rights, and their own backups. Drama Warden does not provide media content or guarantee third-party availability.

## Contributing

See [contribution guidelines](CONTRIBUTING.md), [security policy](SECURITY.md), and [public release checks](docs/PUBLIC-RELEASE.md). CI checks the Rust workspace, bundled provider adapters, frontend build, Docker image, and the full Git history for private material.

## License

MIT. See [LICENSE](LICENSE) and [third-party notices](THIRD-PARTY-NOTICES.md).
