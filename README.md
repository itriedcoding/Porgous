# Porgous

A lightweight, open-source game server management panel written primarily in Rust.

Porgous runs game servers as isolated Docker containers, manages multiple nodes over an authenticated HMAC-signed HTTP protocol, and serves a React management interface embedded in the panel binary. It is designed for small and medium communities that want Pterodactyl-style functionality without PHP, a queue worker, or a separate frontend deployment.

**License:** MIT · **Panel/agent version:** 0.1.0

## What works

- Rust workspace with `porgous` (panel) and `porgous-agent` (node daemon).
- Axum REST API and WebSocket console, SQLite **or** PostgreSQL through one SQL layer.
- Argon2id passwords, TOTP 2FA with recovery codes, session cookies with CSRF protection, scoped API keys, rate limiting, audit logs and RBAC.
- Server lifecycle: create, install, start, stop, restart, kill, delete, recreate containers, crash detection and restart policies.
- Docker resource limits (CPU, memory, swap, pids, block IO), port allocations, environment variables and `{{VAR}}` templating.
- Sandboxed file manager (path traversal and symlink escape protection), uploads/downloads, atomic edits, chmod.
- tar.gz backups with SHA-256 checksums, verified restore, scheduled backups and retention-safe deletion.
- Scheduled tasks with a five-field cron parser and per-task offsets.
- 11 curated eggs: Minecraft Java (Fabric), Minecraft Bedrock, FiveM, Rust, Terraria, ARK, CS:2, Palworld, Valheim, TF2, generic Docker.
- Multi-node support with bearer token + HMAC-SHA256 request signing and a 5-minute replay window.
- React 18 + TypeScript + Tailwind UI embedded into the panel binary (login, overview, servers, live console, files, backups, nodes, users, account).
- Native Android Kotlin client (`android/`): TOTP-capable login, server list, power controls and a live WebSocket console with CPU/memory stats and automatic reconnect.
- Ed25519-verified updater with SHA-256 checksums, binary smoke test, atomic replacement, `.bak` rollback, channels and update history.
- GitHub Actions release workflow for Linux x86_64 and Windows x86_64, `.tar.gz` and `.deb` packaging, checksums, manifest and signature.

## Verified in this environment

- `cargo check --workspace --all-targets` succeeds.
- `cargo test --workspace` passes **43 tests** covering crypto, password policy, TOTP vectors, API keys, node HMAC/replay, eggs, SQLite migrations, file sandbox escapes, backups, cron, updater parsing and rate limiting.
- Production web build completes (`npm run build`, 166 kB JS / 12.6 kB CSS).
- Live smoke test: panel boots with SQLite, serves `/health`, the embedded SPA, `/api/meta`, authenticates an administrator, returns a session and lists servers and eggs.
- Android client builds with Gradle 8.9 against SDK 35: `:app:assembleDebug`, `:app:assembleRelease` and 8 JVM tests (`PanelUrlsTest`) pass. Debug APK 9.9 MB; minified unsigned release APK 1.3 MB.

## Known limitations (not hidden)

- **Docker-backed container operations were not executed in the development VM** because its user cannot access `/var/run/docker.sock`. The Docker runtime is implemented against bollard, but live create/start/console/backup integration must be validated on a host with Docker. The panel reports Docker as unavailable instead of pretending.
- **The Android client was compiled but not run on a physical device or emulator.** Its UI and live console are unverified at runtime; API URL handling, payload decoding and console-event parsing are covered by JVM tests.
- The Android app is a control surface only: it cannot create servers, edit files, manage backups/subusers or configure 2FA. Release APKs must be signed with your own keystore; no signed APK is published.
- **The Tauri desktop application is not included in this revision.** The requirement for a native desktop client remains outstanding.
- **No GitHub release has been published** and no release signing key is configured. The workflow is present; publishing requires a tag, repository write access and `RELEASE_SIGNING_KEY`/`RELEASE_PUBLIC_KEY` secrets.
- Remote-node file/backup operations use the agent API; cross-node integration tests were not run against a second machine.
- SFTP is not implemented. Secure transfers use the authenticated HTTPS file API.
- Only Linux x86_64 and Windows x86_64 are built by CI; Arch packaging is provided as a PKGBUILD, and aarch64 builds are not automated.

## Quick start (development)

```bash
# prerequisites: Rust 1.85+, Node 20+, Docker
cd web && npm ci && npm run build && cd ..
cargo build --release
PORGOUS_DATA_DIR=./data \
PORGOUS_DATABASE_URL='sqlite://./data/porgous.db?mode=rwc' \
PORGOUS_PUBLIC_URL=http://localhost:8080 \
./target/release/porgous serve
```

Open <http://localhost:8080>. On first start an administrator is created; if no password was configured it is written to `./data/initial-admin.txt` with mode `0600`.

## Documentation

- [Installation](docs/INSTALL.md)
- [Configuration](docs/CONFIGURATION.md)
- [API](docs/API.md)
- [Security](docs/SECURITY.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Contributing](docs/CONTRIBUTING.md)
- [Android client](android/README.md)

## Repository layout

```
crates/porgous-core     library: auth, API, runtime, files, backups, updater
crates/porgous-panel    panel binary + embedded web assets
crates/porgous-agent    node agent binary
web/                    React + TypeScript + Tailwind frontend
android/                native Kotlin/Compose Android client
eggs/                   curated server templates (JSON)
config/                 example configuration
packaging/              systemd unit, PKGBUILD, deb builder
scripts/                install script
docs/                   documentation
.github/workflows/      CI and release pipeline
```

## Security

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md). Never expose the Docker socket to the panel process in a multi-user deployment without a rootless/proxy setup; Porgous runs containers through the daemon API and does not pass the socket into game containers.
