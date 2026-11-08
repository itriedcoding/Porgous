# Porgous for Android

Native Kotlin client for a Porgous panel. It uses the same REST and WebSocket API as the web UI: cookie session authentication, CSRF header on mutations, and the server console socket.

## Features

- Sign in with email/password, optional TOTP or recovery code.
- Configurable panel URL, including reverse-proxy subpaths.
- Server list with live status, allocation, egg, node and resource limits.
- Start, restart, stop and kill controls.
- Live console over WebSocket with command input, status events, notices and CPU/memory stats.
- Automatic console reconnect and 10-second server-list refresh.
- Session and panel URL persist in app-private storage; sign out clears the session.

Requires Android 8.0 (API 26) or newer. No Google services are used.

## Build

Requirements: JDK 17 and Android SDK 35 (build-tools 35.0.0).

```bash
cd android
echo "sdk.dir=$ANDROID_HOME" > local.properties
./gradlew :app:testDebugUnitTest :app:assembleDebug
```

The APK is at `app/build/outputs/apk/debug/app-debug.apk`. The Gradle wrapper pins Gradle 8.9.

For a minified release build:

```bash
./gradlew :app:assembleRelease
```

That produces an unsigned APK. Sign it with your own keystore, or add a `signingConfigs` block to `app/build.gradle.kts` and keep the keystore and passwords out of version control:

```bash
apksigner sign --ks /secure/porgous.jks \
  --out porgous-android.apk app/build/outputs/apk/release/app-release-unsigned.apk
```

## Configuration

The app asks for the panel URL at sign-in. Enter `panel.example.com` and the app uses HTTPS, or `http://192.168.1.10:8080` for a LAN panel. A copied `https://panel.example.com/api` URL is normalized. The panel must be reachable from the device; Android has no localhost shortcut to the panel host.

`usesCleartextTraffic` is enabled so self-hosted panels without TLS work on a LAN. Use HTTPS for any panel exposed to the internet; without it the session cookie travels in plaintext.

## API usage

| Action | Request |
| --- | --- |
| Login | `POST /api/auth/login` with `{email,password,totp?}` |
| Restore session | `GET /api/auth/session` |
| List servers | `GET /api/servers` |
| Power | `POST /api/servers/{uuid}/power` with `{"action":"start\|stop\|restart\|kill"}` and `x-csrf-token` |
| Console | `WS /api/servers/{uuid}/ws` with the session cookie |

The console socket sends `{"type":"output\|status\|notice\|stats","data":...}`. Commands are `{"type":"command","data":"..."}`.

## Not included

This client is a focused control surface, not a full replacement for the web panel: it does not create servers, edit files, manage backups/subusers, or configure 2FA. Those remain in the web UI. The app has no offline mode and shows panel errors directly.
