# Changelog

## 0.1.0 — unreleased

Initial implementation.

- Rust workspace: `porgous` panel, `porgous-agent`, shared `porgous-core`.
- SQLite/PostgreSQL data layer with migrations.
- Argon2id auth, sessions, TOTP 2FA, recovery codes, scoped API keys, RBAC, audit logs, rate limiting.
- Docker runtime (bollard), remote agent runtime, signed node transport.
- Server lifecycle, installs, limits, allocations, variables, crash detection, restart policies.
- File manager, backups with checksums and verified restore, cron schedules.
- 11 curated eggs with import/export support.
- React/TypeScript/Tailwind UI embedded in the panel.
- Ed25519-verified updater with checksums, rollback and channels.
- GitHub Actions Linux/Windows build and package workflow.
- Unit tests for crypto, auth primitives, node HMAC, eggs, SQL, files, backups, cron, updater and rate limits.

Known gaps: Android and Tauri clients, automated aarch64 builds, live Docker integration verification and published signed releases.
