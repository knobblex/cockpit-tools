# Codex API Service Handoff

## Scope and runtime

The **Codex API Service** is an authenticated gateway run by the desktop host.
Codex profiles reach it through managed `auth.json` and `config.toml` files.
All production protocol handling uses the native Rust engine. Existing Go/legacy
selections migrate to native while preserving settings and history; the Go
executable and engine switch have been removed.

```text
Codex CLI / selected profile -> local authenticated gateway
                            -> account pool / provider credentials -> upstream
React page -> Tauri commands -> Rust collection/runtime -> native engine
```

**Overview → Forwarding policy** narrows both chat and image candidates before
scheduling. Existing configurations default to all available accounts. Premium-only
mode accepts local OAuth accounts with confirmed team identity and a `prolite`
plan snapshot; unknown identities, personal plans and independent API/official
grants are excluded. Refresh account details to update that snapshot. Selected-account
mode accepts accounts from chat/image pools and explicit Key scopes; an empty
effective list stops forwarding. Save applies the mode while retaining the selection for later
switches. Key scope, model permissions, pending seat barriers, cooldowns and quota protection
still apply. Model discovery, retries and official-to-local fallback use the same
restriction. Internal directed probes and instance gateways keep their own scope.

**Automatic premium seat rotation** is a fourth forwarding mode, experimental and
disabled by default. Configure and enable [seat rotation](codex-premium-seat-rotation.md)
before selecting and saving this mode. Only this mode binds the configured Key's
local OAuth requests to its verified holder and triggers rotation on quota exhaustion.
Other Keys and matching independent API/official grants retain their normal pools.
An unverified holder stops managed OAuth routing; missing or disabled rotation
configuration stops this mode. All, premium-only and selected-account modes ignore
the holder binding and do not trigger rotation. Switching modes retains rotation
settings and history; already-submitted operations still require reconciliation,
and pending source/target barriers remain effective across all modes and Keys.
Route preparation rechecks the current policy, as does rotation before submission.
Existing saved forwarding modes remain unchanged when loading older configurations.

## Source ownership

| Area | Source | Responsibility |
| --- | --- | --- |
| UI | [Service page](../src/pages/CodexApiServicePage.tsx), [service bridge](../src/services/codexLocalAccessService.ts) | Controls and typed Tauri payloads. |
| Contract | [Frontend types](../src/types/codexLocalAccess.ts), [Rust models](../src-tauri/src/models/codex_local_access.rs) | Configuration and state, serialized in camelCase. |
| Commands | [Command boundary](../src-tauri/src/commands/codex_local_access_commands.rs), [registration](../src-tauri/src/lib.rs) | Exposed operations. |
| Runtime | [Coordinator](../src-tauri/src/modules/codex_local_access.rs), [collection migration](../src-tauri/src/modules/codex_local_access_collection.rs), [native bridge](../src-tauri/src/modules/codex_local_access_native.rs) | Persistence, listener lifecycle, profile takeover, quotas and diagnostics. |
| Protocol | [Native engine](../src-tauri/src/modules/codex_native_api/) | HTTP/SSE/WebSocket handling, upstream requests and continuity. |
| Routing | [Native routes](../src-tauri/src/modules/codex_local_access_native_routes.rs), [image routes](../src-tauri/src/modules/codex_local_access_native_images.rs) | Caller scope, model visibility, account eligibility and image selection. |

## Change and diagnosis rules

Request details expose optional **request/response summary recording**, disabled
by default. Enable it under **Settings → Codex** to capture new requests only;
inspect saved summaries in **API Service → request details**.
The [summary builder](../src-tauri/src/modules/codex_native_api/diagnostic_payload.rs)
keeps routing/error fields, array counts, recent input samples and short text
previews instead of full conversation bodies. Allowlisted request/response
headers include correlation IDs and retry hints; authentication and cookie
headers are excluded. Prompt previews can still contain private user text.
Responses are saved separately from requests, including HTTP rejection bodies;
stream snapshots replace the previous snapshot for that attempt and retain its
response headers. An upstream's generic `Bad Request` remains generic in the log.

The [diagnostic store](../src-tauri/src/modules/codex_local_access_request_diagnostics.rs)
defines the rolling age, row and payload-byte limits. Old summaries are evicted
before attempt metadata when the byte budget is exceeded; header storage counts
toward that budget. Clearing summaries retains attempts and usage. Capture and
storage queues are bounded and may drop optional diagnostics under load; they
do not guarantee complete wire recording. Historical bodies are not backfilled.

- Read `codex_local_access_get_state` for service status, endpoint and errors.
  Follow request IDs through `codex_local_access_query_request_logs`; use the
  built-in test only when a real upstream request is authorized.
- Preserve profile takeover backups and the configured endpoint when changing
  activation or credentials. Instance gateways remain isolated by profile.
- Keep model discovery and request rewriting aligned. Test caller/key scope,
  rejected models and account eligibility alongside successful routing.
- Subscription quota cooldown uses the [availability predicate](../src-tauri/src/modules/codex_seat_rotation.rs)
  described by [request-triggered seat rotation](codex-premium-seat-rotation.md#自动流程).
- Derive artifact paths from the application's data directory in the coordinator.
  Generated runtime files can contain credentials; keep them out of logs, issues
  and fixtures. Use the existing redaction and API logging helpers.
- Serialized fields need defaults and recoverable migrations. Follow the
  [query and migration rules](query-and-migration-rules.md) for persistent changes.

## Verification

[Contributing](../CONTRIBUTING.md#-testing) lists the standard checks.
Native integration tests live with the coordinator and protocol modules;
[seat rotation](codex-premium-seat-rotation.md#验证) has an isolated test entry.
Changes to activation, keys, endpoints or credentials also need coverage of
profile takeover and restore. Release and packaging checks are defined in the
[release process](release-process.md).
