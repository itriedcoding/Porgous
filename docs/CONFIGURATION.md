# Configuration

Porgous reads, in order: built-in defaults, a TOML file, then `PORGOUS_*` environment variables. The file is searched at `/etc/porgous/porgous.toml`, `./porgous.toml`, then `./config/porgous.toml`, or passed with `--config`.

A complete example is in `config/porgous.toml.example`.

## Core

| Key | Default | Meaning |
| --- | --- | --- |
| `data_dir` | `./data` | Volumes, backups, eggs, update files |
| `database_url` | `sqlite://./data/porgous.db?mode=rwc` | `sqlite:` or `postgres:` URL |
| `bind` | `0.0.0.0:8080` | Panel listen address |
| `public_url` | `http://localhost:8080` | External URL for cookies and links |
| `panel_name` | `Porgous` | Display name |
| `session_ttl_hours` | `168` | Session lifetime |
| `secure_cookies` | `false` | Add `Secure` to the session cookie |
| `trusted_proxies` | `[]` | Proxy IPs whose forwarded headers are trusted |
| `allow_registration` | `false` | Self-service registration |
| `allow_user_servers` | `true` | Non-admins can create servers |
| `max_upload_bytes` | `536870912` | Upload and request body limit |
| `secret_key` | generated | 32-byte base64/hex key for secret encryption |

The master key encrypts TOTP seeds and node secrets. If it is lost, 2FA and remote nodes must be reconfigured. It is generated at `<data_dir>/secret.key` with mode `0600`.

## Rate limiting

`[rate_limit]` — `enabled`, `requests_per_minute` (300), `auth_per_minute` (10). Login has an additional per-account lockout after five failures.

## Docker

`[docker]` — `enabled`, `host`, `network`, `default_image`, `timezone`, `memory_overhead_mb`, `container_user`, `stop_timeout_secs`, `console_history_lines`.

- `host` may be `unix:///var/run/docker.sock` or a TLS-protected TCP endpoint.
- `container_user` (for example `1000:1000`) is applied when the image supports it.
- `memory_overhead_mb` is added to the game's memory limit for the JVM/runtime.
- `stop_timeout_secs` is used before a forced kill.

## Node

`[node]` — `name`, `bind`, `data_dir`, `token`, `secret`, `sftp_port`. Only the agent uses `bind/data_dir/token/secret`; the panel uses `name` and `sftp_port` for the automatically registered local node.

## Updates

`[updates]` — `enabled`, `repo`, `channel` (`stable`/`prerelease`), `public_key`, `check_interval_hours`, `auto_apply`, `asset_prefix`, `restart_command`.

Updates are never installed when `public_key` is empty. The updater verifies the Ed25519 signature on `SHA256SUMS`, then the SHA-256 of the downloaded binary, runs `--version` as a smoke test, and only then replaces the executable. The previous binary is kept under `porgous-updates/` next to the executable.

Generate a signing key:

```bash
porgous keygen release.key
# prints the public key; store release.key outside the repository
porgous sign release.key SHA256SUMS
```

## TLS

`[tls]` — `cert`, `key`. Both or neither.

## Admin bootstrap

`[admin]` — `email`, `username`, `password`. Used only when the users table is empty. An empty password generates one into `<data_dir>/initial-admin.txt`.

## Environment variables

`PORGOUS_DATA_DIR`, `PORGOUS_DATABASE_URL`, `PORGOUS_BIND`, `PORGOUS_PUBLIC_URL`, `PORGOUS_PANEL_NAME`, `PORGOUS_SECRET_KEY`, `PORGOUS_SECURE_COOKIES`, `PORGOUS_ALLOW_REGISTRATION`, `PORGOUS_ALLOW_USER_SERVERS`, `PORGOUS_DOCKER_HOST`, `PORGOUS_DOCKER_ENABLED`, `PORGOUS_NODE_NAME`, `PORGOUS_NODE_BIND`, `PORGOUS_NODE_DATA_DIR`, `PORGOUS_NODE_TOKEN`, `PORGOUS_NODE_SECRET`, `PORGOUS_UPDATE_REPO`, `PORGOUS_UPDATE_CHANNEL`, `PORGOUS_UPDATE_PUBLIC_KEY`, `PORGOUS_UPDATE_AUTO_APPLY`, `PORGOUS_TLS_CERT`, `PORGOUS_TLS_KEY`, `PORGOUS_ADMIN_EMAIL`, `PORGOUS_ADMIN_USERNAME`, `PORGOUS_ADMIN_PASSWORD`, `PORGOUS_RESTART_COMMAND`.

Use environment variables or a systemd `EnvironmentFile` with mode `0600` for secrets. Do not commit them.

## CLI

```
porgous [serve] [--config FILE]     start the panel
porgous migrate                     apply migrations
porgous admin create --email ... --username ... [--password ...]
porgous admin reset-password --email ...
porgous update --check | [--restart] | --rollback
porgous keygen [secret-file]
porgous sign <secret-file> <file>
porgous version
```

`porgous-agent [serve] [--config FILE]`
