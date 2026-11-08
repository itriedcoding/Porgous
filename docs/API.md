# API

Base path: `/api`. Requests and responses are JSON unless noted. Errors use:

```json
{ "error": { "code": "forbidden", "message": "Administrator access required" } }
```

## Authentication

- Cookie session: `porgous_session` (HttpOnly, SameSite=Lax) plus the `x-csrf-token` header on every non-GET request. Obtain the token from `POST /auth/login` or `GET /auth/session`.
- API key: `Authorization: Bearer porgous_<prefix>_<secret>`. No CSRF header is needed. Keys carry scopes such as `server:read`, `server:control`, `server:console`, `admin`, `*`.

```bash
curl -c cookies -H 'Content-Type: application/json' \
  -d '{"email":"admin@example.com","password":"..."}' \
  https://panel.example.com/api/auth/login

curl -b cookies https://panel.example.com/api/servers
```

## Auth and account

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/auth/login` | `{email,password,totp?}` → user + CSRF, sets cookie |
| POST | `/auth/logout` | Revoke session |
| GET | `/auth/session` | Current user, CSRF, admin flag, API scopes |
| POST | `/auth/register` | Register when enabled |
| POST | `/auth/password` | Change password |
| POST | `/auth/2fa/setup` | Begin TOTP setup (returns otpauth URL + secret) |
| POST | `/auth/2fa/enable` | Confirm TOTP code, returns recovery codes |
| POST | `/auth/2fa/disable` | Disable with password and code |
| PATCH | `/account` | Update username/email |
| GET/POST | `/account/api-keys` | List/create scoped keys |
| DELETE | `/account/api-keys/{id}` | Revoke key |
| GET | `/account/sessions` | List sessions |
| DELETE | `/account/sessions/{id}` | Revoke session |
| GET | `/notifications` | Notifications |
| POST | `/notifications/read-all` | Mark all read |
| POST | `/notifications/{id}/read` | Mark one read |
| DELETE | `/notifications/{id}` | Delete |

## Servers

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/servers` | Servers visible to the caller |
| POST | `/servers` | Create and install a server |
| GET | `/servers/{id}` | Detail, allocations, variables, environment, install log |
| PATCH | `/servers/{id}` | Update settings (owner/admin) |
| DELETE | `/servers/{id}` | Delete container, volume, backups and records |
| POST | `/servers/{id}/power` | `{action: start\|stop\|restart\|kill}` |
| POST | `/servers/{id}/command` | `{command}` to the console |
| GET | `/servers/{id}/logs` | Recent console output |
| GET | `/servers/{id}/metrics` | Metric history |
| GET | `/servers/{id}/ws` | WebSocket console + metrics + status |

Create request:

```json
{
  "name": "Survival",
  "egg_id": "minecraft-java",
  "node_id": "uuid",
  "memory_mb": 2048,
  "disk_mb": 10240,
  "cpu_percent": 200,
  "image": "eclipse-temurin:21-jre",
  "variables": { "MC_VERSION": "1.21.4" },
  "auto_restart": true,
  "restart_policy": "on-failure"
}
```

Server IDs in routes accept the server UUID. The console WebSocket sends JSON events; text/plain is never trusted as HTML.

## Files

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/servers/{id}/files?path=` | List directory + usage |
| GET | `/servers/{id}/files/content?path=` | Read a text file (≤2 MiB) |
| PUT | `/servers/{id}/files/content` | `{path, content}` atomic write |
| POST | `/servers/{id}/files/upload?path=` | Raw body upload |
| GET | `/servers/{id}/files/download?path=` | Download (stream) |
| POST | `/servers/{id}/files/mkdir` | `{path}` |
| POST | `/servers/{id}/files/rename` | `{from,to}` |
| POST | `/servers/{id}/files/chmod` | `{path,mode}` (unix) |
| DELETE | `/servers/{id}/files?path=` | Delete file/folder |

Paths are relative to the server volume. Absolute paths, `..`, NUL bytes and symlinks escaping the volume are rejected.

## Backups, schedules and subusers

| Method | Path | Purpose |
| --- | --- | --- |
| GET/POST | `/servers/{id}/backups` | List/create backup |
| DELETE | `/servers/{id}/backups/{backup_id}` | Delete |
| POST | `/servers/{id}/backups/{backup_id}/restore` | Verify checksum and restore |
| GET | `/servers/{id}/backups/{backup_id}/download` | Download archive |
| GET/POST | `/servers/{id}/schedules` | List/create cron schedule |
| PATCH/DELETE | `/servers/{id}/schedules/{schedule_id}` | Update/delete |
| POST | `/servers/{id}/schedules/{schedule_id}/run` | Run now |
| GET/POST | `/servers/{id}/subusers` | List/add subuser with permissions |
| PATCH/DELETE | `/servers/{id}/subusers/{sub_id}` | Update/remove |
| GET/POST | `/servers/{id}/allocations` | List/attach port |
| DELETE | `/servers/{id}/allocations/{allocation_id}` | Detach port |

## Administration

| Method | Path | Purpose |
| --- | --- | --- |
| GET/POST | `/users` | List/create users |
| PATCH/DELETE | `/users/{id}` | Update/delete |
| POST | `/users/{id}/reset-password` | Reset password |
| GET/POST | `/nodes` | List/create nodes |
| PATCH/DELETE | `/nodes/{id}` | Update/delete |
| POST | `/nodes/{id}/rotate` | Rotate token/secret (shown once) |
| POST | `/nodes/{id}/test` | Health check |
| GET/POST | `/nodes/{id}/allocations` | List/create allocations |
| DELETE | `/allocations/{id}` | Delete allocation |
| GET | `/eggs`, `/eggs/{id}` | List/detail templates |
| POST | `/eggs/import` | Import/validate an egg |
| GET | `/eggs/{id}/export` | Export egg JSON |
| DELETE | `/eggs/{id}` | Delete custom egg |
| GET/PATCH | `/admin/settings` | Settings |
| GET | `/admin/audit` | Audit log |
| GET | `/admin/stats` | Usage counts |
| GET | `/updates` | Update status |
| POST | `/updates/apply` | Verify and install latest |
| POST | `/updates/rollback` | Restore previous binary |
| GET | `/updates/history` | Update history |
| GET | `/meta` | Public panel metadata |
| GET | `/health` (not under `/api`) | Health probe |

## Node agent API

Base `/v1`, all routes except `/v1/health` require `x-porgous-token`, `x-porgous-timestamp` and `x-porgous-signature`. The signature is HMAC-SHA256 over:

```
METHOD\nPATH_AND_QUERY\nUNIX_TIMESTAMP\nSHA256_HEX(BODY)
```

Requests older than 300 seconds are rejected. Endpoints: container create/start/stop/kill/remove/inspect/stats/logs/command/exec, `/install`, `/images`, `/files/*`, `/backups/*`, `/volumes/delete`. The agent is not a public API and should be reachable only by the panel.
