#!/usr/bin/env bash
# Porgous Linux installer. Verifies the signed SHA256SUMS before installing.
#
# Usage:
#   sudo PORGOUS_PUBLIC_KEY='<base64 ed25519 public key>' bash install.sh
#   sudo PORGOUS_VERSION=v0.1.0 PORGOUS_PUBLIC_KEY='...' bash install.sh
set -euo pipefail

REPO="itriedcoding/Porgous"
VERSION="${PORGOUS_VERSION:-latest}"
PUBKEY="${PORGOUS_PUBLIC_KEY:-}"

if [ "$(id -u)" -ne 0 ]; then
  echo "error: run as root (sudo)" >&2
  exit 1
fi
for cmd in curl openssl sha256sum tar; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "error: $cmd is required" >&2; exit 1; }
done
if [ -z "$PUBKEY" ]; then
  echo "error: PORGOUS_PUBLIC_KEY is required so the release signature can be verified." >&2
  echo "Copy the Ed25519 public key from the release notes." >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64) ARCH="x86_64" ;;
  *) echo "error: no prebuilt release for $(uname -m); build from source (docs/INSTALL.md)" >&2; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$VERSION" ] || { echo "error: could not resolve the latest release" >&2; exit 1; }
fi

BASE="https://github.com/$REPO/releases/download/$VERSION"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Downloading Porgous $VERSION..."
curl -fL --retry 3 -o "$TMP/porgous.tar.gz" "$BASE/porgous-${ARCH}-linux.tar.gz"
curl -fL --retry 3 -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS"
curl -fL --retry 3 -o "$TMP/SHA256SUMS.sig" "$BASE/SHA256SUMS.sig"

# Raw Ed25519 key -> SPKI DER -> PEM, then verify the checksums signature.
printf '\060\052\060\005\006\003\053\145\160\003\041\000' > "$TMP/pub.der"
printf '%s' "$PUBKEY" | openssl base64 -d -A >> "$TMP/pub.der"
openssl pkey -pubin -inform DER -in "$TMP/pub.der" -out "$TMP/pub.pem" >/dev/null
openssl pkeyutl -verify -pubin -inkey "$TMP/pub.pem" -rawin \
  -in "$TMP/SHA256SUMS" -sigfile "$TMP/SHA256SUMS.sig" >/dev/null \
  || { echo "error: release signature verification FAILED; refusing to install" >&2; exit 1; }

( cd "$TMP" && grep " porgous-${ARCH}-linux.tar.gz\$" SHA256SUMS | sha256sum -c --strict - ) \
  || { echo "error: checksum verification FAILED; refusing to install" >&2; exit 1; }

tar -xzf "$TMP/porgous.tar.gz" -C "$TMP"
install -Dm755 "$TMP/porgous" /usr/local/bin/porgous
install -Dm755 "$TMP/porgous-agent" /usr/local/bin/porgous-agent

getent group porgous >/dev/null || groupadd --system porgous
getent passwd porgous >/dev/null || useradd --system --gid porgous \
  --home-dir /var/lib/porgous --shell /usr/sbin/nologin porgous
install -d -o porgous -g porgous -m 750 /var/lib/porgous /var/lib/porgous-node
install -d -m 750 /etc/porgous

if [ ! -f /etc/porgous/porgous.toml ]; then
  install -m 640 -o root -g porgous "$TMP/config/porgous.toml.example" /etc/porgous/porgous.toml
  echo "Created /etc/porgous/porgous.toml - review public_url, database_url and admin settings."
fi

install -Dm644 "$TMP/porgous.service" /etc/systemd/system/porgous.service
install -Dm644 "$TMP/porgous-agent.service" /etc/systemd/system/porgous-agent.service 2>/dev/null || true
systemctl daemon-reload

echo
echo "Porgous $VERSION installed."
echo "1. Edit /etc/porgous/porgous.toml"
echo "2. systemctl enable --now porgous"
echo "3. Read /var/lib/porgous/initial-admin.txt for the first password (if generated)."
