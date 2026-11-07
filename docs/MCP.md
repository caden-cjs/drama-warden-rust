# Optional MCP bridge

Drama Warden provides a standalone Rust MCP server over stdio. It is a separate process from the HTTP service and is disabled unless `ENABLE_MCP=true` or `ENABLE_MCP=1` is set. The HTTP service does not expose the MCP transport.

The bridge forwards tool calls to the authenticated REST API, so it uses the same event state machine, plugin boundaries, and storage safeguards as the web/API path. It requires the API service to be reachable and `API_ACCESS_TOKEN` to match the service configuration.

## Run locally

Build the Rust workspace with `cargo build --locked --release --manifest-path rust/Cargo.toml`, start the HTTP service, then start the MCP process with the same API token:

~~~sh
ENABLE_MCP=true \
API_ACCESS_TOKEN='<the same token used by the HTTP service>' \
DRAMA_WARDEN_API_URL=http://127.0.0.1:18889 \
./rust/target/release/mcp-bridge
~~~

The default MCP API URL is `http://127.0.0.1:8080`, which suits a locally running HTTP service. When Docker Compose publishes the service on another host port, set `DRAMA_WARDEN_API_URL` to that address.

## Configure an MCP client

Configure the client to launch `mcp-bridge` as its stdio server. For example:

~~~json
{
  "mcpServers": {
    "drama-warden": {
      "command": "/absolute/path/to/drama-warden/rust/target/release/mcp-bridge",
      "args": [],
      "env": {
        "ENABLE_MCP": "true",
        "API_ACCESS_TOKEN": "<the same token used by the HTTP service>",
        "DRAMA_WARDEN_API_URL": "http://127.0.0.1:18889"
      }
    }
  }
}
~~~

Keep the API token in the MCP client's private configuration. The MCP process uses stdout only for the protocol and reports startup/configuration errors to stderr.

## Tools and side effects

The bridge exposes show and event reads, metadata search, resource discovery, follow-up preview, event queue/retry/process, automation status, and a one-shot scan. Search and preview do not save media. Queueing creates a pending event and reserves episode write claims. `process_event` writes selected media through the configured storage provider and retains the existing uncertain-write handling. The MCP bridge does not expose direct provider calls or deletion operations.

The Rust bridge uses the configured API token and HTTP service. The Docker image includes it as `drama-warden-mcp-bridge` for MCP clients that can launch a process in the application environment.
