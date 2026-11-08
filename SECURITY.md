# Security policy

## Reporting a vulnerability

Do not open a public issue for security problems. Email the maintainer listed on the repository profile, or use GitHub's private "Report a vulnerability" advisory feature. Include the affected version, reproduction steps and impact. You will receive an acknowledgement as soon as possible.

## Supported versions

Only the latest release receives security fixes while the project is pre-1.0.

## Security model

- Passwords: Argon2id (`argon2` defaults: 19 MiB, 2 iterations, 1 lane), unique salts, minimum 10 characters with mixed character classes.
- Sessions: 256-bit random tokens, only SHA-256 hashes stored, `HttpOnly` + `SameSite=Lax` cookies, optional `Secure`, CSRF header required on cookie-authenticated mutations.
- API keys: `porgous_` prefix, SHA-256 hashed at rest, scoped (`server:read`, `server:control`, `admin`, wildcards), optional expiry.
- 2FA: TOTP SHA-1/6 digits/30 s with ±1 step drift, encrypted secret, single-use recovery codes hashed with SHA-256.
- Node transport: bearer token compared by hash plus HMAC-SHA256 over method, path+query, timestamp and body SHA-256; ±300 s replay window; constant-time comparison.
- Secrets at rest: ChaCha20-Poly1305 with a per-install 32-byte key.
- File access: lexical component validation, canonicalization, root containment, symlink target checks, atomic writes, 2 MiB edit limit and configurable upload limit.
- Backups: tar.gz archives, SHA-256 checksums, restore rejects absolute paths, `..`, device files, hardlinks and symlinks; expansion is size-capped.
- Containers: no Docker socket mount, no privileged mode, memory/CPU/pids/IO limits, dedicated bridge network and per-server volume.
- HTTP: strict CSP, `X-Frame-Options: DENY`, `nosniff`, referrer policy, permissions policy, body limits, per-IP rate limits.
- Audit logs for authentication, server lifecycle, files, backups, users, nodes and updates.

## Deployment guidance

1. Serve the panel over HTTPS. Use `secure_cookies = true` and a TLS proxy or direct TLS.
2. Keep the Docker socket accessible only to the panel user; never expose it to game containers, users, or the public network.
3. Run the panel as a dedicated unprivileged user (`porgous`), not root, unless Docker group access requires it. Rootless Docker is recommended.
4. Set `updates.public_key`; unsigned updates are refused by design.
5. Back up `<data_dir>` and the database together; the master key is required to restore node credentials.
6. Restrict registration and server creation for public panels.
7. Review `audit_logs` and notifications for repeated login failures or unexpected power actions.
8. Keep Rust dependencies patched; run `cargo audit` in CI.

## Known gaps

- Live Docker isolation has not been penetration-tested in this environment.
- No external security audit has been performed.
- SFTP is not implemented; the HTTPS file API is the supported transfer path.
