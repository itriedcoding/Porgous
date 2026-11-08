# Security configuration

See the repository [SECURITY.md](../SECURITY.md) for the policy and vulnerability reporting. This page covers hardening.

## TLS

Use direct TLS (`[tls]`) or a reverse proxy with `public_url = "https://..."` and `secure_cookies = true`. Set `trusted_proxies` only to proxies you control. Never expose the panel over plain HTTP on the internet.

## Accounts

- The first administrator is created at bootstrap. If no password is configured, it is written to `<data_dir>/initial-admin.txt` (mode `0600`). Change it after first login.
- Disable registration after creating accounts (`allow_registration = false`).
- Require 2FA for administrators. TOTP secrets are encrypted with the master key; store recovery codes offline.
- Use API keys with the smallest scope and an expiry. Revoke unused keys.

## Docker and node isolation

- Run the panel as a dedicated user. Membership in the `docker` group is effectively root-equivalent on the host; prefer rootless Docker or a socket proxy that only exposes the endpoints Porgous needs.
- Never mount `/var/run/docker.sock` into a game server. Porgous does not do this.
- Game containers run with memory, swap, CPU, pids and IO limits, no privileged mode, and a per-server volume under `<data_dir>/volumes/<uuid>`.
- Remote agents should listen on a private network or behind TLS. Tokens and secrets are shown once and stored encrypted by the panel.

## Backups and secrets

Back up the database, `<data_dir>/volumes`, and `<data_dir>/secret.key`. Without the master key, encrypted node credentials and TOTP seeds cannot be recovered. Backups contain game data and should be protected like the server itself.

## Updates

Configure `updates.public_key` with the release signing key. The updater refuses unsigned or checksum-mismatched binaries. Restrict write access to the panel binary and its update directory.

## Auditing

Review `/api/admin/audit` and notifications. Authentication failures, file writes, power actions, user/node changes, backups and update installs are recorded. Rotate or export logs before retention cleanup (365 days for audit rows).

## Rate limiting

Keep `rate_limit.enabled = true`. The login guard locks an account for five minutes after five failures. Put a WAF or proxy rate limit in front of public panels as defense in depth.

## Hardening checklist

- [ ] HTTPS and secure cookies
- [ ] Unique admin password and 2FA
- [ ] Registration disabled
- [ ] `updates.public_key` set
- [ ] Docker access restricted / rootless
- [ ] Agent on private network or TLS
- [ ] Database credentials unique and not in the TOML world-readable
- [ ] `secret.key` backed up and `0600`
- [ ] Audit log monitoring
- [ ] `cargo audit` and image updates scheduled
