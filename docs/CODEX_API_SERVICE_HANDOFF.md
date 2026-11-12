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

Optional [premium seat rotation](codex-premium-seat-rotation.md) coordinates an
existing workspace seat and native routing. It is experimental and disabled by default.

**Overview → Forwarding policy** narrows both chat and image candidates before
scheduling. Existing configurations default to all available accounts. Premium-only
mode accepts local OAuth accounts with confirmed team identity and a `prolite`
plan snapshot; unknown identities, personal plans and independent API/official
grants are excluded. Refresh account details to update that snapshot. Selected-account
mode accepts accounts from chat/image pools and explicit Key scopes; an empty
effective list stops forwarding. Save applies the mode while retaining the selection for later
switches. Key scope, model permissions, seat barriers, cooldowns and quota protection
still apply. Model discovery, retries and official-to-local fallback use the same
restriction. Internal directed probes and instance gateways keep their own scope.

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
