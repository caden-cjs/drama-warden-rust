# Plugin management and configuration API RFC

**Status: implemented in progress.** The Rust host now discovers bundled plugin commands independently of provider credentials, persists per-plugin configuration, exposes protected read/validate/update/reset endpoints, encrypts secret fields, and renders schema-driven forms in the panel. External plugin package discovery and failed-registration status cards remain deployment follow-up work.

## Summary

The Rust host owns plugin discovery, per-plugin settings, enablement, lifecycle, and capability routing. A plugin's presence must not depend on provider credentials being present. The authenticated management API and application database are the normal place to configure plugins. Environment variables remain for host-level deployment settings and temporary compatibility with existing installs.

Installing or replacing executable plugin code remains an operator action. This API manages trusted plugins that the host already discovered; it is not a package manager or remote code execution interface.

## Goals and boundaries

- Discover and register installed plugins independently of whether their provider credentials are present.
- Let the panel and other API clients inspect a plugin's configuration requirements without exposing credentials.
- Make the authenticated API and application database the normal place to configure plugins, validate settings, and enable providers.
- Keep one versioned plugin protocol for Node, Rust, Python, and other child-process implementations.
- Keep plugin installation and host process-command selection under deployment control. The API must not accept arbitrary plugin process commands, binaries, packages, or source code.
- Keep workflow decisions, application database access, and event state in the Rust host. A provider receives only its declared configuration and capability input.

All routes use the existing `Authorization: Bearer <API_ACCESS_TOKEN>` authentication. The current application has no separate plugin-admin role, so a valid API token can manage plugins and their configuration.

## Existing routes

`GET /api/plugins` returns the registered plugin manifests and runtime state. Existing routes enable/disable a plugin, move its order, run `monitor()`, and run `reset()`. Manifests declare a `configSchema`; the host resolves database overrides first, then supported compatibility environment values, then manifest defaults. Secret values are never returned.

Bundled plugin commands are registered independently of provider credentials. A missing required value marks that plugin `configured: false` and keeps it unavailable for provider calls without preventing other plugins from starting. Registration failures are currently written to protected service logs; a dedicated failed-registration row in the plugin list remains future work.

The proposed routes add configuration management for plugins already registered at process start:

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/plugins/:id/config` | Read the public schema, per-field configured state, and safe non-secret values |
| `POST` | `/api/plugins/:id/config/validate` | Validate a candidate configuration without persisting it |
| `PATCH` | `/api/plugins/:id/config` | Save a configuration override and apply it to the registered plugin |
| `DELETE` | `/api/plugins/:id/config` | Remove saved overrides and return to environment/default values |

These routes do not install or register new plugin executables. Add a third-party plugin through `plugins.commands` or the deployment's plugin command configuration, then restart the service so the host can inspect and validate its manifest. Built-in plugins should be discoverable from the packaged plugin catalog without requiring their environment variables to be present.

For newly configured external plugins, add a stable `id` to the deployment command descriptor. The host requires the returned manifest ID to match it. This lets the API display a safe registration failure even when a process cannot start or return its manifest. Existing descriptors without `id` remain supported during migration; failures before manifest discovery are reported only as registration errors in protected service logs.

Track these states independently:

| State | Meaning |
| --- | --- |
| Registered | The host found the plugin executable and accepted its manifest |
| Configured | Every required setting is present and passes host validation |
| Enabled | An operator enabled the configured plugin for provider calls |
| Healthy / authorized | The plugin's last monitor check reported its current external-service state |

Plugins with no required settings are configured by default and the UI shows “无需配置”. A provider that needs settings cannot be enabled until it is configured. Authorization is reported separately from configuration because valid settings do not guarantee a valid account session.

The plugin list should report `configured`, operator-requested `enabled`, runtime `active`, and the most recent safe health summary independently. If an enabled plugin becomes unconfigured after a configuration or schema change, keep the operator's enabled preference but set `active: false` and report the reason. A later valid configuration may activate it without changing the saved enablement preference.

## Manifest configuration schema

The current field schema has a key, type, and required flag. Extend each field with optional UI and validation metadata while preserving the existing fields:

```json
{
  "version": 1,
  "fields": [
    {
      "key": "cliPath",
      "type": "path",
      "label": "CLI executable",
      "description": "Path to the installed provider CLI.",
      "required": true,
      "secret": false,
      "scope": "deployment"
    },
    {
      "key": "cookie",
      "type": "password",
      "label": "Login cookie",
      "description": "Used only to import an existing provider login.",
      "required": false,
      "secret": true,
      "scope": "runtime"
    }
  ]
}
```

Supported field types should be `text`, `url`, `path`, `password`, `boolean`, `integer`, and `select`. `secret` defaults to `true` for `password` and `false` for other types; plugins may not mark a password field as non-secret. `scope` is either `runtime` (editable through the API) or `deployment` (read-only in the API and managed by the service operator). It defaults to `runtime`. Add bounded metadata such as `min`, `max`, `options`, `placeholder`, `group`, and a non-secret `default` only where applicable. Do not support defaults on secret fields, arbitrary regular expressions, remote schemas, dynamic JavaScript, or HTML in manifests.

The host validates the schema itself: unique safe keys, supported types, bounded label/help text, valid option lists, and type-appropriate limits. The API returns these fields as data for the panel to render; it never renders plugin-provided markup. Executable paths, child-process arguments, authorization-state directories, and other host process settings must use `deployment` scope and cannot be changed through the plugin configuration API.

## Configuration API behavior

### `GET /api/plugins/:id/config`

Return the manifest schema, where each field is accompanied by `configured`, `source`, and an optional safe value. Never return secret values, even when the current process obtained them from the environment. Include the overall configuration and activation state:

```json
{
  "pluginId": "quark",
  "schemaVersion": 1,
  "configured": true,
  "enabled": true,
  "active": true,
  "fields": [
    {
      "key": "cliPath",
      "type": "path",
      "label": "CLI executable",
      "required": true,
      "configured": true,
      "source": "deployment",
      "value": "/opt/quarkclouddrive/scripts/quark-drive.cjs"
    },
    {
      "key": "cookie",
      "type": "password",
      "label": "Login cookie",
      "required": false,
      "secret": true,
      "configured": true,
      "source": "database"
    }
  ]
}
```

For secret fields, omit `value` completely. A panel may show “已配置” and offer an empty replacement field. Do not return masked fragments of a secret: they are not useful for configuration and can leak information.

### `POST /api/plugins/:id/config/validate`

Accept a candidate patch and validate field names, required values, types, bounds, and cross-field constraints declared by the manifest against the effective configuration. Unknown keys and writes to `scope: "deployment"` fields are rejected. A plugin may implement an optional side-effect-free `validateConfig` lifecycle method for provider-specific checks. This route must not persist values, change the active plugin, write media, or return secret input.

Request:

```json
{
  "config": {
    "recycleDirectory": "/DramaWarden-Recycle/Quark"
  }
}
```

Response:

```json
{
  "valid": true,
  "pluginId": "quark",
  "issues": []
}
```

Validation errors identify a field and a stable code, but never echo the submitted value or raw provider error text.

### `PATCH /api/plugins/:id/config`

Accept `{ "config": { ... } }` and merge only the supplied fields into the saved database overrides. Omitted fields are retained. Require all manifest-required fields in the effective configuration after applying the precedence rules below. Reject invalid values before storing anything.

For secret fields, a non-empty submitted value replaces the encrypted secret. An omitted secret preserves its current value. `null` explicitly clears an optional field; clearing a required field fails validation. Empty strings are rejected for secrets so a blank form cannot erase a credential accidentally. `DELETE /api/plugins/:id/config` removes every database override for that plugin.

Apply changes by constructing and initializing a candidate plugin process before switching traffic. If initialization fails, retain the previous active process and configuration. When the plugin is disabled, save the validated configuration and initialize it when re-enabled. Drain in-flight calls before stopping or replacing a process. Return only safe field status and a stable error code.

Successful response:

```json
{
  "pluginId": "quark",
  "applied": true,
  "fields": [
    { "key": "cliPath", "configured": true, "source": "database" },
    { "key": "cookie", "configured": true, "source": "database" }
  ]
}
```

If a plugin is disabled, `applied` may be `false` with `pendingActivation: true`; no secret value is returned.

### `DELETE /api/plugins/:id/config`

Delete the plugin's database overrides and restore effective values from supported deployment compatibility values and manifest defaults. Do not delete provider authorization files or volumes. Return the resulting safe field statuses. If required values are now missing, preserve the saved enablement preference but set the plugin inactive until it is configured again.

## Registration and startup lifecycle

The Rust host maintains two independent inputs:

1. **Plugin catalog:** built-in plugin executables shipped with the application and external plugin commands explicitly installed by the operator.
2. **Plugin configuration:** validated per-plugin values stored by the management API, with manifest defaults and legacy environment fallback.

Environment variables must not add or remove a provider from the catalog. At startup, the host should discover each catalog entry, request and validate its manifest, load its saved enablement and configuration, then initialize only configured and enabled plugins. An incomplete plugin remains visible with `configured: false`; it does not block other plugins or application startup. A plugin with no required fields starts configured, but should still follow its saved enablement state.

The plugin process lifecycle is:

```text
discover command → request manifest → validate manifest → resolve configuration
    ├─ missing required fields → registered, inactive, configured=false
    ├─ configured but disabled → registered, inactive, enabled=false
    └─ configured and enabled → initialize(config) → active
```

`initialize(config)` must be local and idempotent: it prepares client state but does not write media or mutate business records. External account checks belong to `monitor()`. If a plugin command cannot start or returns an invalid manifest, mark that catalog entry failed and continue loading other plugins; expose a safe error code in plugin status and log details only to protected server logs. New deployment command descriptors should have a stable plugin `id`, verified against the manifest, so this failed entry can be shown in the API.

For a provider call, the host checks `enabled && configured && active` before invoking the capability. The call fails closed with `PLUGIN_DISABLED`, `PLUGIN_CONFIG_REQUIRED`, or `PLUGIN_UNAVAILABLE` as appropriate. The event processor retains ownership of retry and side-effect semantics.

## Configuration schema contract

Each field has a stable machine key and a human-facing label. The proposed field contract is:

| Property | Required | Meaning |
| --- | --- | --- |
| `key` | yes | Stable identifier used in API objects and environment compatibility mapping |
| `type` | yes | `text`, `url`, `path`, `password`, `boolean`, `integer`, or `select` |
| `label` | yes | Short plain-text form label |
| `description` | no | Plain-text help text; bounded length |
| `required` | no | Defaults to `false`; determines `configured` |
| `secret` | no | Defaults to true for `password`; controls storage and API redaction |
| `scope` | no | `runtime` or `deployment`; defaults to `runtime` |
| `default` | no | Non-secret typed default only |
| `options` | select only | Bounded list of `{ value, label }` choices |
| `min`, `max` | no | Type-appropriate numeric/string bounds |
| `placeholder`, `group` | no | Plain-text panel hints, never behavior or executable input |

`deployment` fields are read-only through `/config`: plugin process command, child arguments, volume mounts, and process timeout belong in protected deployment configuration. The Quark and 115 bundled adapters deliberately expose their installed CLI path and authorization state directory as runtime plugin settings so an administrator can configure them in the panel. Those paths must be absolute; changing a CLI path selects a local program that the trusted adapter will execute. The API never accepts a new plugin process command or arbitrary plugin code.

Schema versioning is independent of plugin API versioning. The host stores the schema version with saved values. A plugin may expose an optional `migrateConfig` lifecycle method to migrate a prior version; the host validates the migrated output before use. If migration fails, preserve the stored encrypted data, mark the plugin `configurationNeedsMigration`, and keep it inactive until an operator resolves it. Do not discard unknown fields during a schema upgrade.

## API request and response rules

- All paths are under the protected `/api/plugins` namespace and use the existing Bearer token.
- Plugin IDs and field keys use the same restricted identifier syntax as manifests; paths are never interpolated into shell commands.
- API JSON uses `camelCase`. Responses contain stable public codes and no raw provider errors.
- `GET /api/plugins` includes manifest schema and safe state summary, not configuration values. `GET /api/plugins/:id/config` adds non-secret safe values and source metadata.
- `POST .../config/validate` validates a patch over the current effective configuration. It does not persist or activate the result.
- `PATCH .../config` merges the patch with current saved values, validates the effective configuration, persists it, and applies it if enabled.
- `DELETE .../config` removes the whole plugin override record. It does not disable the plugin or delete its authorization state.
- Add `revision` to the config response. Mutations include `expectedRevision`; return `409 PLUGIN_CONFIG_REVISION_CONFLICT` if another client changed the configuration after it was read.
- A secret is never returned. `null` explicitly removes a saved optional secret; empty strings are rejected. Omitted fields are unchanged by `PATCH`.
- Deployment-scoped settings are reported as `source: "deployment"`, `editable: false`. Other sources are `database`, `manifestDefault`, `legacyEnvironment`, or `unset`.

Suggested validation issue shape:

```json
{
  "valid": false,
  "pluginId": "pan115",
  "issues": [
    { "field": "minIntervalMs", "code": "PLUGIN_CONFIG_OUT_OF_RANGE" },
    { "field": "cookie", "code": "PLUGIN_CONFIG_REQUIRED" }
  ]
}
```

`issues` must not echo submitted values. A validation route may call optional plugin `validateConfig`, which receives a candidate config but must be side-effect free. It must not call save/move/remove, import auth state, or send notifications. If a plugin cannot validate externally without a side effect, it should return schema-only validation and leave connectivity checks to `monitor()`.

## Atomic configuration apply

For an enabled plugin, a valid update is applied with a candidate process:

1. Validate schema, required fields, field scope, and `expectedRevision`.
2. Build the candidate effective configuration and encrypt secret fields.
3. Start a candidate process using the already registered deployment command; do not accept a new command from the API.
4. Request its manifest and require the same plugin ID and compatible plugin/API/schema versions.
5. Run `validateConfig` if implemented, then `initialize(candidateConfig)`.
6. Persist the new config transactionally only after candidate initialization succeeds.
7. Atomically route new calls to the candidate, drain calls on the old process, then reset and stop the old process.

If any step before persistence fails, discard the candidate and keep the old config and process active. If the plugin is disabled, validate and persist the config without starting it; the next enable request initializes it. Process replacement must never resend an in-flight mutating capability automatically.

Candidate startup may perform provider authentication in existing adapters. Plugin implementations should move such behavior into explicit, idempotent setup/authorization operations and keep `initialize` side-effect free. For this first API version, no generic authorization endpoint is defined; use `monitor()` for status and retain documented provider-specific authorization steps. Quark OAuth remains an official CLI operation. The 115 Cookie is a sensitive setting; after the CLI has imported it and persisted authorization state, the operator may clear it through the config API.

## Persistence, encryption, backup, and audit

Keep plugin enablement/order separate from plugin config. Reuse the existing `application_setting` `plugins` record for enablement and order. Add a dedicated config record per plugin rather than mixing credentials into that settings JSON:

```text
plugin_configuration
  plugin_id          primary key
  schema_version     integer
  config_json        non-secret runtime fields only
  secrets_json       authenticated-encryption envelopes only
  revision           monotonically increasing integer
  updated_at
```

Use the same logical schema in SQLite and MySQL. The current implementation uses AES-256-GCM via `ring` with a dedicated 32-byte hex key supplied outside the database as `PLUGIN_CONFIG_ENCRYPTION_KEY`. Store a format version, random nonce, and ciphertext. Bind `plugin_id`, field key, and schema version as associated data so ciphertext cannot be moved between fields or plugins. Never derive this key from `API_ACCESS_TOKEN`.

Generate a deployment key with `openssl rand -hex 32` and provide it through the protected service environment. Keep it outside the database and backups. Losing it makes saved plugin secrets unreadable. The panel never receives this key.

If a saved secret cannot be decrypted, the plugin is unconfigured and inactive. Return `PLUGIN_CONFIG_SECRET_DECRYPT_FAILED`; do not fall back to an environment value or initialize with an empty credential. Key rotation is an explicit offline migration: decrypt with the old key, re-encrypt with the new key, verify counts, then switch the deployment key.

Database backups and portable database exports contain encrypted secret envelopes, never the encryption key. Restoring them requires the matching key. Portable export/import must validate encrypted records and preserve them byte-for-byte; if a key is unavailable, import metadata and non-secret fields but report that encrypted plugin settings were skipped, never silently replace them with empty values.

The host writes a metadata-only audit record after each successful configuration save or reset: plugin ID, changed field keys, which keys are secret, outcome, revision, and timestamp. Failed validation attempts are not yet persisted to this audit table. Never record values, ciphertext, Cookie strings, authorization files, or raw plugin errors. Do not put configuration changes into `plugin_call_log`.

## Plugin authoring and provider examples

Every plugin implements the same protocol and declares its own config schema in `manifest`. It receives only manifest-declared effective values in `initialize.config`. It must not read ambient environment variables for ordinary runtime settings. The host validates config independently before passing it to the child. Third-party plugins should declare:

- Required and optional fields with types, constraints, labels, and secret/scope metadata.
- Defaults only for non-secret values.
- Stable error codes for invalid or missing settings.
- `validateConfig` only when validation can be performed without side effects.
- Idempotent `initialize`, `reset`, and `monitor` behavior.
- Which fields are deployment-owned and which can safely change through the API.

Quark and 115 fields under this model:

| Provider field | Scope | Secret | Purpose |
| --- | --- | --- | --- |
| Quark `cliPath` | runtime | no | Absolute path to the installed official CLI |
| Quark `stateDirectory` | runtime | no | Persistent OAuth state directory |
| Quark `recycleDirectory` | runtime | no | Target for `remove(mode="recycle")` |
| 115 `cliPath` | runtime | no | Absolute path to the installed `115cli` |
| 115 authorization directory | runtime | no | Persistent CLI login state |
| 115 `cookie` | runtime | yes | Optional import input; clear it after CLI authorization is persisted |
| 115 `minIntervalMs` | runtime | no | Minimum CLI call spacing |
| 115 `wafCooldownMs` | runtime | no | WAF cooldown interval |

The configuration API does not install either CLI, run Quark browser OAuth, or delete authorization files. `remove` semantics remain provider-specific and explicit: Quark recycles; 115 permanently removes.

## Migration from environment-based configuration

The migration must preserve existing deployments and avoid making credentials disappear:

1. First ship a host that discovers bundled plugin manifests independently of configuration and continues accepting current environment variables as legacy sources.
2. Preserve current enablement/order from the existing `application_setting` `plugins` object. Previously configured plugins remain enabled after upgrade; newly discovered optional plugins start disabled.
3. Expose each legacy environment value only as `configured: true, source: "legacyEnvironment"`; never copy secrets to responses or logs.
4. Let operators save an API override. Database values then take precedence over legacy environment values for that field.
5. Keep environment fallback available for one compatibility window. Show the field as “由部署配置提供” until the operator saves a database value.
6. After a documented deprecation window, remove provider-specific environment fallbacks. Host-level process settings such as DB credentials, API token, bind address, and plugin executable command list remain environment/deployment configuration.

No automatic secret import is required for first release. If a later migration copies legacy secrets into the encrypted table, require the encryption key first, make the migration transactional and idempotent, and verify counts without printing values.

## Errors and audit

Use stable codes such as `PLUGIN_NOT_FOUND`, `PLUGIN_CONFIG_INVALID`, `PLUGIN_CONFIG_REQUIRED`, `PLUGIN_CONFIG_SECRET_KEY_MISSING`, `PLUGIN_CONFIG_SECRET_DECRYPT_FAILED`, `PLUGIN_CONFIG_REVISION_CONFLICT`, `PLUGIN_CONFIG_APPLY_FAILED`, and `PLUGIN_CONFIG_VALIDATION_UNSUPPORTED`. Validation details may identify a field and expected type, but must not include submitted values.

The HTTP status mapping should be consistent: `400` malformed request, `401` invalid/missing API token, `404` unknown plugin, `409` revision or lifecycle conflict, `422` invalid or incomplete configuration, and `502` plugin process/validation failure. Database failures use `500` with a generic message and safe code.

## Implementation and acceptance sequence

1. Add independent built-in plugin discovery and explicit plugin states; missing provider config must not fail application startup.
2. Extend and validate manifest config schemas with backward-compatible defaults.
3. Add config persistence, revision handling, AEAD secret storage, backup/portable-transfer handling, and metadata audit.
4. Implement read, validate, patch, and reset routes with redaction and field-scope enforcement.
5. Implement candidate process activation and drain/swap behavior; preserve event write-claim and no-automatic-retry semantics.
6. Build the panel form from manifests and show registered/configured/enabled/active/authorized states distinctly.
7. Quark and 115 CLI paths, state directories, and runtime options are editable through their plugin configuration forms; installing the CLI and preserving its authorization directory remain operator responsibilities.
8. Migrate other bundled plugins one at a time and deprecate environment fallbacks after compatibility checks.

Acceptance requires tests for manifest validation, unconfigured startup isolation, config precedence, secret encryption/decryption and redaction, secret clearing, optimistic concurrency, failed candidate rollback, disabled-plugin config updates, process draining, schema migration, backup/import behavior, and plugin call audit redaction. Verify these with fake child processes; do not use real cloud writes as configuration tests.

