# Provider development

The Rust service loads providers as supervised child processes using the language-neutral protocol in [Rust plugin protocol](RUST-PLUGIN-PROTOCOL.md). Provider processes perform external I/O and normalize results; follow-up rules, event states, episode matching, and write claims remain in the Rust application. The included adapter command and supported provider IDs are listed in [external plugins](../plugins/README.md).

## Rust plugin configuration

Bundled plugin commands are discovered independently of provider credentials. Configure runtime fields in the authenticated `/plugins` panel or through `/api/plugins/:id/config`; the Rust host stores them in SQLite/MySQL and returns only non-secret values. Existing environment names remain a compatibility source. Secret fields require `PLUGIN_CONFIG_ENCRYPTION_KEY` as a 32-byte hex value supplied by the deployment and are encrypted before database storage. Secret values are never returned by the API.

Plugin host commands remain operator-managed through `plugins.commands` in `config/config.yaml` or `DRAMA_WARDEN_PLUGIN_COMMANDS`. Those command descriptors do not accept API updates. Third-party plugin commands are appended to the bundled catalog, and their declared `id` must match the manifest ID.

The Rust service starts registered provider adapters as supervised child processes. Configure executable commands with `plugins.commands`; do not use the former in-process `plugins.modules` loader. Each active adapter receives only its declared effective configuration and communicates through the versioned JSON-lines protocol. Missing required configuration leaves that plugin inactive without blocking other providers.

External providers implement the language-neutral JSON Lines protocol documented in [Rust plugin protocol](RUST-PLUGIN-PROTOCOL.md). The Rust echo example in `rust/crates/app-server/examples/plugin-echo.rs` shows a complete minimal process. The workspace package in `packages/plugin-sdk` supports the bundled Node adapters' internal capability contracts; it is not the Rust extension API.

Bundled Node provider adapters use the helpers in `backend/plugins/contract.mjs` and the workspace SDK in `packages/plugin-sdk`. Providers must not write application tables or decide event outcomes.

Subtitle providers use `defineSubtitlePlugin()` and implement `search({ query, languages, seasonNumber, episodeNumber, limit })` plus `download({ candidateId })`. Search returns candidates with stable provider-local `id`, `fileName`, `language`, and explicit season/episode numbers when available. Download returns `{ fileName, content }`, where `content` is a `Buffer` or `Uint8Array`; the core enforces format, size, ZIP, and episode-match checks. The core tries providers in plugin registration order and does not allow providers to choose storage paths or write files.

The child-process protocol uses the same provider semantics. Its `download` result uses `contentBase64`, and a storage provider can implement the optional `writeSidecar` capability to receive validated subtitle bytes.

## Resource capability shapes

Resource providers implement `search({ query, aliases, mediaType, storageProvider, seasonNumber, episodeNumber, limit })` and return either an array or `{ items: [...], truncated: false }`. Each item must include a `shareUrl` (or `url`); `title` and stable provider `sourceId` are optional. `storageProvider` identifies the destination adapter, so a provider can filter candidates before the core previews them. Set `truncated: true` when the result list is incomplete. The core bounds the result count, removes duplicate share URLs, and checks each candidate through the selected storage provider's share listing and follow-up preview. Discovery does not create events or write media files. The preview can refresh local metadata and apply the existing exact-match identity linking rule. Implement providers with `defineResourcePlugin()` and map `search` to the `resource` capability domain.

The built-in `local-resource` provider indexes video paths below the configured local share root, bounded to 5,000 entries and depth 12, and refreshes the index every 60 seconds. It only matches exact normalized show/alias directory names, season-directory suffixes, or filenames with a recognizable episode suffix. Incomplete indexes are marked truncated, so scheduled automation will not queue their candidates.

## Local filesystem provider

Docker Compose enables the built-in `local-storage` storage provider and `local-resource` search provider by default through `storage.local.enabled` in `config/config.example.yaml`. They read shares from a host directory mounted read-only at `/local-shares`; storage writes media under `/app/data/media` in the persistent application volume. Set `LOCAL_STORAGE_SHARE_ROOT_HOST` to select another host source directory. Set `storage.local.enabled: false` in YAML, or `LOCAL_STORAGE_ENABLED=false` in the process environment, to disable both providers. For a non-Docker run, configure `storage.local.shareRoot` to a readable source directory.

Share links use `local-share:/<relative-path>`, where the path is relative to the configured share root. A share can point to a file or directory. Add a storage root through `POST /api/shows/:id/storages`; its `rootPath` is a POSIX path relative to the provider's configured media root, such as `/shows/example`.

The local provider refuses paths containing traversal segments and refuses symbolic links. It requires an explicit file selection for `save`, bounds directory listings, and reports when a listing was truncated. Its only removal mode is `permanent`. `clean` preserves files and directories by default; deleting child directories or nonmatching files requires explicit options.

The local provider also declares optional `writeSidecar({ videoPath, fileName, content })`. It accepts only non-empty SRT/ASS/SSA/VTT bytes up to 8 MiB, writes beside the exact video asset with exclusive-create semantics, refuses symbolic links and never replaces an existing file. This is an optional storage capability outside the shared `storageContract v2` core list. Quark and 115 do not currently implement it because their adapters do not expose local subtitle upload; a provider must explicitly implement and verify the capability before subtitle jobs can target it.

## Built-in subtitle providers and worker

OpenSubtitles is registered when `OPENSUBTITLES_API_KEY` is configured in `.env`. It uses the documented `/api/v1/subtitles` search and `/api/v1/download` flow, sends the key only to the API host, validates the temporary download host, and bounds every response. Set `subtitles.openSubtitlesUserAgent` if the account requires a specific application identification string. The Assrt HTML-search adapter is opt-in with `subtitles.assrtEnabled: true`; its website markup can change, so leave it disabled when it is unavailable.

The background worker is enabled by default and processes only explicitly queued jobs. Queue requests require one existing canonical episode and one present video asset; multiple video versions are treated as ambiguous and skipped. Jobs are unique per storage/season/episode and survive restart. Network/provider failures retry at most three times with backoff before any storage write. Each claim uses a random lease token; stale workers cannot update or write for a job reclaimed by another worker. Once `writeSidecar` is called, an unconfirmed result becomes `uncertain`; operators should inspect the target directory before taking further action. Existing matching sidecars are left untouched. Enabled providers are tried in plugin registration order.

## Built-in metadata providers

The built-in TVmaze provider is registered on every start and requires no credential. It searches TV series, reads details and seasons, and resolves each season through the TVmaze seasons and episode endpoints. Requests use a distinct User-Agent, a 12 second timeout, and a local sliding-window limit below TVmaze's published rate ceiling. The provider caches season IDs briefly between detail and episode requests; `reset()` clears that cache. Its plugin self-check requests the public `/shows/1` endpoint, so a passing result confirms the API is reachable from the application host. A failed check returns a stable metadata error code without exposing the URL or response body.

The TMDB provider is registered when `TMDB_API_KEY` is nonempty. The value is sent as the TMDB v3 `api_key` query parameter and is never returned in provider results or plugin manifests. Search supports TV and movies; details and episode manifests support TV series. Its plugin self-check requests `/configuration` with the configured key. Provider failures are reduced to stable codes such as `METADATA_TIMEOUT`, `METADATA_RATE_LIMITED`, and `METADATA_UPSTREAM_ERROR`; the self-check does not expose the key, URL, or response body.

When both providers are present, TMDB is registered first and TVmaze follows it for metadata resolution. Search queries all enabled providers and reports partial failures per provider. The authenticated `GET /api/metadata/search?q=...` endpoint returns candidates but does not link them; callers confirm one through the show's metadata identity endpoint.

## Built-in Feishu notifications

Set `FEISHU_WEBHOOK_URL` or `UPDATE_EVENT_WEBHOOK_URL` to register the optional `feishu-notification` provider. It subscribes to `workflow.event.status-changed` and sends completed, skipped, or failed follow-up events to the separate event endpoint when configured, otherwise as a Feishu card. `FEISHU_WEBHOOK_SECRET` signs Feishu requests; `UPDATE_EVENT_WEBHOOK_TOKEN` is sent as a Bearer credential to the event endpoint. Both URLs must use HTTPS. Secrets are read only from the process environment and are not included in plugin manifests or health responses.

Notifications have an eight-second request timeout and stable error codes: `NOTIFICATION_INVALID_CONFIG`, `NOTIFICATION_NOT_CONFIGURED`, `NOTIFICATION_TIMEOUT`, `NOTIFICATION_NETWORK_ERROR`, `NOTIFICATION_HTTP_ERROR`, `NOTIFICATION_INVALID_RESPONSE`, and `NOTIFICATION_REJECTED`. The notifier does not retry a failed request because a timeout can happen after Feishu has accepted the message. Notification failures are reported in the workflow callback result and do not change the persisted follow-up event status.

## CloudSaver resource search

The bundled `cloudsaver` process adapter preserves the existing CloudSaver login and `/api/search` flow. Set `CLOUDSAVER_URL`, `CLOUDSAVER_USER`, and `CLOUDSAVER_PASS` in the private environment to auto-register it when `plugins.commands` is empty. If you configure an explicit command list, add the CloudSaver command from [external plugins](../plugins/README.md) yourself. Search results are normalized to `{ items, truncated }` and filtered to the selected storage provider before the Rust core previews share links. The adapter caches its login token in process memory and clears it on `reset()`; credentials and tokens are not returned in health responses or plugin audit records.

## WPS public query resource

The `wps-query` process adapter reads a public WPS query application. Set `WPS_QUERY_URL` to a URL in the form `https://web.wps.cn/etapps/query/q/{application-id}`; the project's public query application is the default. It does not use a WPS account, cookie, or personal login. The adapter reads the app's published encryption configuration and campaign metadata, then performs read-only search requests through the fixed `f-api.wps.cn` host. It rejects other application hosts and returns normalized resource links for the Rust core to validate. WPS's public page protocol is not a stable server API; health checks report upstream changes without attempting writes or authentication workarounds.

## Built-in SmartStrm indexer

Set `indexer.smartstrm.baseUrl` and `SMARTSTRM_WEBHOOK_TOKEN` to enable the optional indexer. An explicitly configured `taskName` is used as a fallback; follow-up normally selects the matching task from storage provider and show category, such as `quark_strm_电视剧`, `quark_动漫`, or `115_综艺`. The provider sends the SmartStrm webhook event `{ "event": "a_task", "task": { "name": "..." } }` after a successful media save and requires a JSON response with `success: true`. It does not log the webhook URL, token, or response body, and it does not retry a request whose outcome is uncertain. A failed or unconfirmed response keeps the follow-up event from being marked processed. Health checks send `{ "action": "ping" }` with a 15-second timeout.

Leave the URL and token unset to skip index refresh. For other indexers, use an external `indexer` plugin that maps `storageId` or `path` to the provider's task model.

## Built-in cloud storage providers

The application and workflow core are Rust. The bundled Quark and 115 storage adapters are currently Node.js protocol-bridge plugins, and they invoke the provider CLIs; these two adapters are not Rust-native yet. This keeps their provider-specific IDs and CLI output inside the adapter while Rust owns scheduling, episode selection, write claims, and event state. A future Rust adapter can use the same plugin protocol and `storageContract v2` without changing those core rules.

Cloud storage is optional and disabled in a default image. Neither CLI is included unless its build option is enabled. Bundled cloud plugin manifests are listed even when their CLI or authorization is unavailable; the plugin remains inactive until its CLI path and required settings are configured. Extra `plugins.commands` entries extend the bundled catalog.

| Provider | Adapter and CLI | Enable/build | Non-secret configuration | Authorization state |
| --- | --- | --- | --- | --- |
| Quark | Node.js adapter + official Quark CLI 1.0.25 | Build with `INCLUDE_QUARK_CLI=true`; set CLI path in the Quark plugin configuration, normally `/opt/quarkclouddrive/scripts/quark-drive.cjs` | Set authorization state directory (normally `/app/data/.quarkclouddrive`) and recycle directory in the Quark plugin configuration | Complete the official OAuth login once; persist `/app/data/.quarkclouddrive` across container rebuilds |
| 115 | Node.js adapter + Python `115cli==0.3.4` | Build with `INCLUDE_115_CLI=true`; set CLI path in the 115 plugin configuration, normally `/usr/local/bin/115cli` | Set `minIntervalMs` (default 3000) and `wafCooldownMs` (default 300000) in the 115 plugin configuration | Supply Cookie in the 115 plugin configuration for initial import; persist `/app/data/.115cli/credentials` |

Both build options default to `false`; their pinned dependencies and license notes are described in [third-party notices](../THIRD-PARTY-NOTICES.md). Quark's CLI archive is SHA-256 checked and the image does not run the upstream install script or fetch a moving version. The 115 CLI and dependencies are installed from a CPython 3.12 hash-locked wheel file for Linux x86_64/aarch64. Review the notices before enabling either adapter.

The same settings can be supplied through the compatibility environment variables `QUARK_CLI_PATH`, `QUARK_STATE_DIR`, `QUARK_RECYCLE_DIR`, `PAN115_CLI_PATH`, `PAN115_MIN_INTERVAL_MS`, and `PAN115_WAF_COOLDOWN_MS`. YAML values are loaded by the Rust host and mapped to adapter configuration. `PAN115_COOKIE` is a secret; keep it in a secret manager or protected environment until it has been saved in the plugin configuration page, never in source control or issue reports.

Open `/plugins`, choose Quark or 115, and configure the CLI path, home/state paths, and runtime options there. The panel labels each current source (`现有环境配置` or `插件数据库`). The 115 Cookie is never shown: paste it from the protected environment source into the password field and save it with `PLUGIN_CONFIG_ENCRYPTION_KEY` configured; after checking authorization, remove the old `PAN115_COOKIE` value from the deployment environment. Environment fallback remains available for compatibility.

For the bundled Quark CLI, run `docker compose exec drama-warden node /opt/quarkclouddrive/scripts/quark-drive.cjs login` and complete the official OAuth flow. The CLI state is stored under `/app/data/.quarkclouddrive` in the persistent `quark-auth` volume. Quark `remove(mode="recycle")` moves items into the configured `storage.quark.recycleDirectory`.

For 115, after enabling `INCLUDE_115_CLI=true` and rebuilding, set `PAN115_COOKIE` in the private `.env` file. The adapter imports it lazily on its first operation; the 115 CLI then stores its credential beneath `/app/data/.115cli/credentials` in the persistent `pan115-auth` volume. Remove the cookie from `.env` after successful authorization. The provider status never returns the cookie. 115 supports only `remove(mode="permanent")`; review that policy before configuring it as a storage location.

Share-file IDs are stable hashes of the share URL and provider file ID, so queued events remain resolvable after a process restart without returning raw provider IDs. Before saving, each adapter re-lists the share and verifies the selected IDs, paths, names, and sizes. Storage directory listings recurse when requested and mark bounded or provider-truncated results as incomplete. The 115 CLI directory listing is capped at 500 entries per directory; a directory at that limit is reported as truncated, and follow-up processing refuses the incomplete snapshot.

The providers use conservative defaults for `clean`: files and child directories are kept unless the caller explicitly sets `deleteDuplicates` or `deleteSubdirs`. A CLI operation timeout or partial write is not retried by the follow-up workflow; inspect the target directory and write claims first.

## Storage capability shapes

All actions receive `(input, context)` and return normalized objects. The current follow-up processor uses:

| Capability | Input | Expected result or behavior |
| --- | --- | --- |
| `list` | `{ path, options: { recursive } }` | `{ path, files, total, truncated }`; file paths use the provider's configured root namespace |
| `listSharedFiles` | `{ shareUrl, options? }` | `{ files, total, truncated }`; each file has stable `id`, `name`, relative `path`, `size`, and `mtime` |
| `save` | `{ shareUrl, destinationPath, selection }` | Save exactly the selected files; return `{ ok, savedCount }` |
| `mkdir` | `{ path }` | Create the directory recursively when needed |
| `move` | `{ sourcePath, destinationPath }` | `destinationPath` is the complete target path |
| `rename` | `{ path, newName }` | Change only the basename |
| `renameMany` | `{ items: [{ path, newName }] }` | Rename the listed files; report partial failure as an error |
| `remove` | `{ path, mode }` | Accept only a mode listed in `storageContract.removeModes` |
| `clean` | `{ path, mode, options }` | Apply the same explicit removal mode to selected children |
| `status` | `{}` | Return normalized provider status |
| `writeSidecar` (optional) | `{ videoPath, fileName, content }` | Create one supported subtitle beside the exact video; fail if the target already exists |

Listings used for follow-up must set `truncated: false` only when complete. Share file IDs must remain stable between queueing and processing so a queued event cannot silently switch to another file. A provider must reject unsupported removal modes instead of substituting a different behavior. `recycle` and `permanent` are distinct operations.

## Metadata capability shapes

Metadata providers implement `search({ query, mediaType, limit })`, `detail({ externalId, mediaType })`, and `episodes({ externalId, seasonNumber })`. Search/detail candidates use `id`, `externalId`, `providerId`, `mediaType`, and `name`; the cross-provider type also permits optional `originalName`, `overview`, `firstAirDate`, `year`, `status`, `language`, `genres`, `posterPath`, `backdropPath`, `voteAverage`, and `externalUrl` fields. Each provider fills the fields it can supply. Detail results may include seasons. Episode results use the canonical fields `seasonNumber`, `episodeNumber`, and `airDate`; external IDs remain strings and are paired with the provider ID. Provider failures should use stable error codes and avoid returning credentials or raw request details.

When an action or `initialize()` / `reset()` hook throws, the host records or returns only its stable error code and a generic `plugin operation failed` error. Error codes may contain 1–64 uppercase ASCII letters, digits, and underscores; missing or invalid codes become `PLUGIN_ERROR`. The host does not forward the provider's raw message or cause to API responses or startup logs. `monitor()` and `onEvent()` failures are reduced to stable error codes. Core workflow decisions use host-generated state and error context; provider error codes cannot mark an event skipped by matching a core code such as `DESTINATION_CONFLICT`.

Plugin module import and factory failures are also reduced to a generic startup message and safe error code. Do not depend on raw exception messages for diagnostics; use stable codes and the host's plugin ID/context in your own private diagnostics without logging configuration values.

The host serializes lifecycle maintenance against capability calls. `reset()` pauses new calls and waits for active calls to finish; disabling a plugin persists the disabled state, rejects new calls, drains active calls, and then runs `reset()`. Re-enabling runs `initialize()` before the plugin becomes available. A failed disable cleanup leaves the plugin disabled and reports `PLUGIN_RESET_FAILED`, so the implementation should make `reset()` safe to retry.

Before contributing a provider, document its configuration, rate limits, identity stability, failure codes, reset behavior, and any side effects. Keep credentials in environment or private runtime configuration, never in manifests or logs.
