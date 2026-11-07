# Rust plugin process protocol

Rust plugins run as supervised child processes and communicate over UTF-8 JSON Lines on stdin/stdout. This boundary does not expose Rust ABI, application database handles, or provider-specific types.

## Framing and lifecycle

- The host starts a configured executable with its declared arguments. It clears the child environment and passes only `PATH` for executable lookup.
- Each input and output frame is one JSON object followed by `\n`. Frames are limited to 24 MiB; stdout is reserved for protocol frames and stderr is discarded. The larger bound accommodates a base64 encoded subtitle download while keeping each child response bounded.
- Every request has `requestId`, `method`, `input`, and optional `correlationId`. Every response echoes `requestId` and contains exactly one of `result` or `error`.
- Startup begins with `method: "manifest"`. The result must be a valid `PluginManifest` with protocol version 1, plugin API version 2, a supported kind, valid subscriptions, and unique `{ name, domain, description }` capabilities. Metadata, resource, subtitle, workflow, and storage plugin kinds must declare their required capabilities. Storage providers must declare `storageContract` version 2 with the shared core capabilities and explicit `recycle` or `permanent` removal modes.
- The host then sends `method: "initialize"` with a `config` object. Configuration values are resolved from manifest-declared `DRAMA_WARDEN_PLUGIN_<PLUGIN_ID>_<FIELD>` environment variables; they are sent only in this request and never included in manifests, API responses, or audit rows.
- `invoke` input contains `capability`, `input`, and a limited call context. The host checks the capability against the manifest before dispatch and records only plugin ID, capability, source, correlation ID, success, stable error code, and elapsed time.
- Lifecycle methods are `initialize`, `reset`, `monitor`, and `onEvent`. The host calls `onEvent` only for enabled plugins whose manifest subscribes to the event type. Status notifications use `workflow.event.status-changed` and receive a reduced event containing status, local show/event IDs, season, and episode numbers; share links, credentials, raw errors, and full stored payloads are excluded. Monitor output is reduced to `ok`, optional `configured` and `authorized`, and a validated `errorCode` before it reaches the API.

The host bounds every request with the descriptor's `timeoutMs` (100 ms through 120 seconds; default 30 seconds). If a request times out or its response cannot be confirmed, the process is killed and the host reports a stable error with `sideEffectMayHaveCompleted: true`. The host does not silently retry a plugin operation. Error responses contain a stable uppercase `code` and may set `sideEffectMayHaveCompleted` when the plugin cannot prove that a write did not complete.

## Configuration

Set `plugins.commands` in `config/config.yaml` or `DRAMA_WARDEN_PLUGIN_COMMANDS` in the process environment to a list containing only executable paths, argument arrays, and optional timeouts. A non-empty environment value overrides YAML. Configuration schemas are declared by each child process manifest. For example:

```yaml
plugins:
  commands:
    - command: /opt/drama-warden/plugins/example-provider
      args: []
      timeoutMs: 30000
```

The equivalent environment value is JSON:

```json
[
  {
    "command": "/opt/drama-warden/plugins/example-provider",
    "args": [],
    "timeoutMs": 30000
  }
]
```

For plugin ID `example-provider` and field `apiKey`, set `DRAMA_WARDEN_PLUGIN_EXAMPLE_PROVIDER_API_KEY`. Undeclared variables under that plugin prefix, missing required fields, invalid URLs, duplicate IDs, invalid manifests, and unsupported protocol versions stop startup. The application does not persist plugin secrets. Plugin command paths and their configuration must be protected as trusted executable code.

## Example

`rust/crates/app-server/examples/plugin-echo.rs` implements the protocol in Rust. It demonstrates the frame shape and manifest without contacting external services. Plugins may use any language that can read and write JSON Lines.

Subtitle plugin `download` results return `{ "fileName": "episode.srt", "contentBase64": "..." }`. Storage providers that declare the optional `writeSidecar` capability receive `{ "videoPath": "...", "fileName": "episode.srt", "contentBase64": "..." }`. Both values use standard padded base64; the host enforces the subtitle size and format limits before and after decoding. Plugins must not return or choose a destination path.
