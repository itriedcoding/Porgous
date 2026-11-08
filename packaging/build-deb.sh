#!/usr/bin/env bash
# Build a .deb from release binaries.
# Usage: build-deb.sh <version> <amd64|arm64> <panel-binary> <agent-binary> <output-dir>
set -euo pipefail

VERSION="${1:?version}"
ARCH="${2:?arch}"
PANEL="${3:?panel binary}"
AGENT="${4:?agent binary}"
OUT="${5:-dist}"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

install -Dm755 "$PANEL" "$ROOT/usr/bin/porgous"
install -Dm755 "$AGENT" "$ROOT/usr/bin/porgous-agent"
install -Dm644 packaging/porgous.service "$ROOT/usr/lib/systemd/system/porgous.service"
install -Dm644 packaging/porgous-agent.service "$ROOT/usr/lib/systemd/system/porgous-agent.service"
install -Dm644 config/porgous.toml.example "$ROOT/etc/porgous/porgous.toml"
install -Dm644 LICENSE "$ROOT/usr/share/doc/porgous/copyright"
install -Dm644 README.md "$ROOT/usr/share/doc/porgous/README.md"
mkdir -p "$ROOT/var/lib/porgous" "$ROOT/var/lib/porgous-node"
mkdir -p "$ROOT/DEBIAN"

cat > "$ROOT/DEBIAN/control" <<EOF
Package: porgous
Version: $VERSION
Section: admin
Priority: optional
Architecture: $ARCH
Depends: docker.io | docker-ce, adduser, openssl
Maintainer: Porgous contributors
Homepage: https://github.com/itriedcoding/Porgous
Description: Lightweight Rust game server management panel
 Multi-node game server management with Docker isolation, signed updates,
 backups, file management and a React web interface.
EOF

cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
getent group porgous >/dev/null || groupadd --system porgous
getent passwd porgous >/dev/null || useradd --system --gid porgous --home /var/lib/porgous --shell /usr/sbin/nologin porgous
chown -R porgous:porgous /var/lib/porgous /var/lib/porgous-node
usermod -aG docker porgous 2>/dev/null || true
systemctl daemon-reload || true
echo "Porgous installed. Edit /etc/porgous/porgous.toml, then: systemctl enable --now porgous"
EOF
chmod 755 "$ROOT/DEBIAN/postinst"

cat > "$ROOT/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e
systemctl stop porgous.service >/dev/null 2>&1 || true
EOF
chmod 755 "$ROOT/DEBIAN/prerm"

mkdir -p "$OUT"
dpkg-deb --build --root-owner-group "$ROOT" "$OUT/porgous_${VERSION}_${ARCH}.deb"
echo "built $OUT/porgous_${VERSION}_${ARCH}.deb"
