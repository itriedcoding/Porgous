# Installation

Porgous has two components:

- `porgous` — panel, API and web UI (one binary).
- `porgous-agent` — optional agent for remote nodes. The panel can run servers on its own host without it.

## Requirements

| Component | Minimum |
| --- | --- |
| Panel | 1 vCPU, 512 MB RAM, 1 GB disk |
| Node | Docker Engine 24+, 1 vCPU per ~2 game servers |
| Rust build | 1.85+ and a C toolchain |
| Database | SQLite 3.35+ (bundled) or PostgreSQL 14+ |

Docker must be installed and running. The panel user needs access to the Docker API. Do **not** publish the Docker socket over TCP without TLS and authentication.

## Ubuntu 22.04/24.04 and Debian 12+

```bash
sudo apt update
sudo apt install -y curl ca-certificates openssl
curl -fsSL https://raw.githubusercontent.com/itriedcoding/Porgous/main/scripts/install.sh -o /tmp/install-porgous.sh
# Set the release public key printed in the release notes:
sudo PORGOUS_PUBLIC_KEY='<base64-ed25519-public-key>' bash /tmp/install-porgous.sh
```

The script installs `/usr/local/bin/porgous`, creates the `porgous` system user, `/etc/porgous/porgous.toml`, `/var/lib/porgous`, and a systemd unit. It verifies `SHA256SUMS` and, when a public key is supplied, its Ed25519 signature before installing.

Start and inspect:

```bash
sudo systemctl enable --now porgous
sudo journalctl -u porgous -f
sudo cat /var/lib/porgous/initial-admin.txt   # first run only
```

## Arch Linux

```bash
cd packaging
makepkg -si
sudo systemctl enable --now porgous
```

Or use the install script above on Arch; it only needs `curl`, `openssl` and systemd.

## Windows 10/11

1. Install Docker Desktop with the WSL 2 backend.
2. Download `porgous-x86_64-windows.exe` and `porgous-agent-x86_64-windows.exe` from the latest release.
3. Put them in `C:\Program Files\Porgous`.
4. Create `C:\Program Files\Porgous\porgous.toml` from `config/porgous.toml.example`, using a Windows path:

```toml
data_dir = "C:/ProgramData/Porgous"
database_url = "sqlite://C:/ProgramData/Porgous/porgous.db?mode=rwc"
bind = "0.0.0.0:8080"
public_url = "http://localhost:8080"
```

5. Run `porgous.exe serve` from an elevated terminal, or register it as a service with NSSM/WinSW.

Docker Desktop must be running under the same user. Remote nodes are supported; Windows containers are not used for game servers.

## PostgreSQL

```sql
CREATE USER porgous WITH PASSWORD 'change-me';
CREATE DATABASE porgous OWNER porgous;
```

```toml
database_url = "postgres://porgous:change-me@127.0.0.1:5432/porgous"
```

Run `porgous migrate` after changing the URL. Migrations are also applied on start.

## TLS

Direct TLS:

```toml
[tls]
cert = "/etc/porgous/tls/fullchain.pem"
key = "/etc/porgous/tls/privkey.pem"
```

Behind a reverse proxy (recommended), leave TLS unset and set `public_url` to the HTTPS URL, `secure_cookies = true`, and add the proxy address to `trusted_proxies`. Caddy example:

```
panel.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

The console WebSocket upgrades through the same proxy without extra configuration.

## Remote node

1. On the node, install `porgous-agent` and its unit.
2. Generate the node in **Nodes → Add node**. The panel shows the token and secret **once**.
3. Put them in the node's `[node]` section and set `bind`, `data_dir`.
4. Ensure the panel can reach the agent over HTTPS (or a private network/VPN).
5. Put the agent behind TLS: set `[tls]` on the agent or use a reverse proxy.

The agent refuses to start without a token and secret of at least 24 characters.

## Building from source

```bash
git clone https://github.com/itriedcoding/Porgous.git
cd Porgous
cd web && npm ci && npm run build && cd ..
cargo build --release
sudo install -m755 target/release/porgous /usr/local/bin/
sudo install -m755 target/release/porgous-agent /usr/local/bin/
```

The release binary embeds the built web assets, so build the frontend before Cargo.

## Updating and rollback

```bash
porgous update --check
sudo porgous update            # verifies signature + checksum, installs
sudo porgous update --rollback # restores the previous binary
```

Set `updates.public_key` to the project's Ed25519 key. Without it, the panel refuses to install updates. `PORGOUS_RESTART_COMMAND` (or `updates.restart_command`) can restart the service automatically.

## Uninstall

```bash
sudo systemctl disable --now porgous
sudo rm /etc/systemd/system/porgous.service /usr/local/bin/porgous
# Data is intentionally kept:
# /var/lib/porgous, /etc/porgous
```
