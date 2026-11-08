# Contributing

## Development setup

```bash
rustup toolchain install 1.85
cd web && npm ci && npm run build && cd ..
cargo test --workspace
cargo run --bin porgous
```

SQLite and a temp data directory are enough for API work. Docker is required for container features.

## Before opening a pull request

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
cd web && npm run build
```

Keep changes focused, add tests for behavior changes, and do not commit secrets, `data/`, `target/`, `node_modules/` or generated credentials.

## Code layout

- `crates/porgous-core/src/auth.rs` — sessions, TOTP, API keys, authorization.
- `crates/porgous-core/src/runtime/` — runtime trait, Docker, remote agent, test runtime.
- `crates/porgous-core/src/server_ops.rs` — server lifecycle and backups.
- `crates/porgous-core/src/api/` — REST handlers.
- `crates/porgous-core/src/scheduler.rs` — background jobs.
- `crates/porgous-core/src/updater.rs` — signed releases.
- `crates/porgous-agent` — node HTTP service with HMAC verification.
- `web/src/App.tsx` — UI. Keep dependencies minimal.

## Eggs

Add a JSON file under `eggs/`. It is embedded into the panel and seeded into `<data_dir>/eggs` on first start. Validate by running the egg tests:

```bash
cargo test -p porgous-core eggs::tests::embedded_eggs_are_valid
```

Eggs must use maintained images, avoid host mounts, document required licenses, and never download from untrusted URLs.

## Security

Do not weaken path checks, signature verification, CSRF, rate limits or authentication to make a test pass. Report vulnerabilities privately per [SECURITY.md](../SECURITY.md).

## Commit messages

Use a short imperative subject, explain the reason in the body, and reference issues where applicable.
