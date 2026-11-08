# Troubleshooting

## The panel starts but `/health` reports `"docker": false`

The Docker daemon is unreachable. Check:

```bash
docker info
systemctl status docker
ls -l /var/run/docker.sock
```

The user running `porgous` must be able to reach the socket. Test with the exact service user:

```bash
sudo -u porgous docker info
```

If the panel runs in a container, mount the socket or use a TCP endpoint with TLS (`docker.host`). Porgous reports the failure in health and logs; it does not silently fake container actions.

## `database is locked` (SQLite)

SQLite allows one writer. Heavy imports or many nodes on one panel should use PostgreSQL. Ensure the database file and its directory are writable by the panel user and stored on a local filesystem, not NFS.

## Login redirects back to the sign-in page

- `public_url` must match the browser URL exactly (scheme/host/port).
- If HTTPS is terminated by a proxy, set `secure_cookies = true` and add the proxy IP to `trusted_proxies`.
- Cookies are `HttpOnly` and `SameSite=Lax`; cross-site embeds will not work.
- Clear stale cookies, then sign in again.

## CSRF errors

Cookie-authenticated `POST/PATCH/DELETE` requests need `x-csrf-token` from `/api/auth/session`. API keys do not need it. The bundled UI handles this automatically.

## Server install fails

Open the server detail and inspect `install_log`. Common causes:

- Image cannot be pulled (registry rate limit, typo, network).
- Egg install script uses a tool absent from its image.
- Disk quota exceeded.
- Docker cannot start a container with the requested port (already allocated).

Test manually:

```bash
docker run --rm -it --entrypoint sh <image>
```

## Server exits immediately

- Check the console/logs and the exit code.
- The startup command may reference a missing file (for example `server.jar`).
- Minecraft requires `eula=true` in `eula.txt`.
- Java memory must fit inside the container limit; leave overhead for the JVM.
- FiveM/ARK/Palworld need a license or large disk; read the egg notes.

## Console is empty or delayed

Local nodes attach to the Docker log stream. Remote nodes poll logs every second by design, so output can lag up to ~1 s. Verify the server is running and that the agent is reachable (`POST /api/nodes/{id}/test`).

## Node requests fail with 401

- The token/secret in the agent config must match the node credentials exactly.
- The node clock must be within 5 minutes of the panel (HMAC timestamps).
- If credentials were rotated, copy the new values to the agent and restart it.
- Confirm the proxy forwards `x-porgous-token`, `x-porgous-timestamp` and `x-porgous-signature` unchanged.

## Backups fail or restore is rejected

- Ensure free space and that the server volume exists.
- Restores verify the SHA-256 checksum; a corrupt archive is refused.
- Archives containing absolute paths, `..`, device files or links are refused by design.
- Check that the backup belongs to the same server.

## Updates are refused

The updater requires a configured Ed25519 public key and a valid `SHA256SUMS.sig`. Errors are recorded in update history and logs. Verify the release signing key matches the configured key, and that the asset for this platform exists (`porgous-<arch>-<os>`).

If the binary was replaced but the service still runs the old code, restart it. Use `porgous update --rollback` to restore the previous binary.

## Windows-specific issues

- Run Docker Desktop as the same user/session as the panel.
- Use `C:/...` paths in the TOML.
- The updater renames the running executable before replacing it; antivirus may block this. Add an exclusion for the install directory.
- Register the service with NSSM/WinSW so it restarts after updates.

## Logs

```bash
sudo journalctl -u porgous -f
sudo journalctl -u porgous-agent -f
RUST_LOG=debug ./porgous serve
```

Do not enable debug logging permanently on a public panel; request bodies and URLs may appear in logs.
