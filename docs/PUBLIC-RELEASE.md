# Public release boundary

The public repository contains the Rust application service, Vue frontend, plugin protocol, bundled provider adapters, documentation, and reproducible build configuration. Its Git history starts from a clean public source snapshot and does not inherit earlier application-server commits.

The application HTTP service, database layer, workflow decisions, scheduling, and MCP bridge are implemented in Rust. JavaScript retained in the repository is limited to the frontend toolchain, CLI/provider process adapters, and plugin SDK tooling. The bundled providers run behind the language-neutral Rust plugin protocol.

## Excluded material

- Environment files, private YAML configuration, credentials, cookies, tokens, webhook URLs, and authorization state
- Database files, database exports, runtime logs, media, subtitles, STRM files, and share listings
- Docker backups, agent state, editor settings, and machine-specific deployment paths
- Third-party CLI binaries; optional upstream tools are fetched at pinned versions and verified during image builds

`.gitignore`, `.dockerignore`, and `.npmignore` cover local runtime state and common private formats. `scripts/check-public-boundary.mjs` scans source files for private paths and common credential patterns. `scripts/check-git-history.mjs` scans every reachable commit, including files removed or changed in later commits. CI fetches complete Git history before running this check.

## Release verification

Before publishing changes:

1. Review all staged source, configuration, dependencies, and generated artifacts.
2. Run `cargo fmt --all -- --check`, `cargo test --workspace --locked`, `cargo check --workspace --all-targets --locked`, `npm run check`, `npm test`, `npm run check:plugin-sdk`, `npm run check:licenses`, `npm run check:public-boundary`, and `npm run check:git-history`.
3. Run Rust formatting, workspace tests, and all-target checks.
4. Build the frontend and Docker image, then run the default-image smoke check.
5. Review the resulting commit history and repository visibility before pushing.

The release scanner is a guard against common mistakes, not a substitute for reviewing the actual files and Git objects being published.
