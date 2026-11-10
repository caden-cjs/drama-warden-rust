# External plugins

`plugins.commands` configures Rust child process plugins. Add standalone protocol-v1 executables here and reference their container paths from `config/config.yaml`; mount this directory read-only in Compose. The process contract, configuration allowlist, and JSON Lines examples are in [Rust plugin protocol](../docs/RUST-PLUGIN-PROTOCOL.md).

When `plugins.commands` is empty, the host registers local storage and local resource search when `LOCAL_STORAGE_ENABLED=true`, and registers optional adapters when their required environment fields are present. A non-empty command list is explicit: include every provider you need in that list, including local adapters.

The Docker image also includes a Node protocol bridge for the project's existing isolated provider adapters. Example entries in `config/config.yaml`:

```yaml
plugins:
  commands:
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, cloudsaver]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, wps-query]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, local-storage]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, local-resource]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, quark]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, pan115]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, smartstrm]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, feishu-notification]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, opensubtitles]
    - command: node
      args: [backend/plugins/node-protocol-bridge.mjs, assrt]
```

Each adapter declares only its own capabilities. Configure values through the existing provider environment fields (`CLOUDSAVER_URL`, `LOCAL_STORAGE_ROOT`, `QUARK_CLI_PATH`, `PAN115_COOKIE`, `SMARTSTRM_BASE_URL`, and so on); the Rust host passes only fields declared by each manifest to its child process. These bridge adapters preserve the provider integration behind the process boundary while the Rust service owns HTTP requests, database state, scheduling, and workflow decisions.

The bundled Quark and 115 adapters are Node.js plugins today; they invoke the official Quark CLI and the Python `115cli`, respectively. They are not native Rust implementations. Build flags, executable paths, authorization setup, persistent state volumes, and each provider's removal behavior are documented in [Built-in cloud storage providers](../docs/PROVIDERS.md#built-in-cloud-storage-providers).
