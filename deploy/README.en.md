# FarHelm V0.9.0 deployment and lifecycle

[简体中文](README.md) · [English](README.en.md)

Formal programs target Ubuntu 24.04 x86_64, or a compatible system with systemd and glibc 2.39+. Hub needs Caddy or an equivalent HTTPS reverse proxy. Agent is outbound-only, opens no port, and needs no sudo.

## Hub

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-hub-linux-x86_64 -o farhelm-hub
chmod +x farhelm-hub
sudo ./farhelm-hub install
```

When configuration is missing, the program asks only for the administrator username and password. For non-interactive installation:

```bash
sudo env \
  FARHELM_ADMIN_USER="admin" \
  FARHELM_ADMIN_PASSWORD="a-random-password-of-at-least-12-characters" \
  ./farhelm-hub install
```

V0.5.0 upgrades directly with `update`; SQLite session, pairing, project, and schedule tables are created idempotently on restart. Old TOTP and token fields remain for local rollback, but V0.8.0 does not require TOTP. A backup is still recommended before the first V0.3.0 cross-generation install:

```bash
sudo cp /var/lib/farhelm/farhelm.db /var/lib/farhelm/farhelm.db.v0.3.bak
sudo cp /etc/farhelm/hub.toml /etc/farhelm/hub.toml.v0.3.bak
sudo ./farhelm-hub install
sudo farhelm-hub doctor
curl -f http://127.0.0.1:8787/api/v1/health
```

Existing dedicated tokens are imported automatically. Agents still using the shared migration token are marked “Pairing required”; create an eight-digit code in the Console and run `farhelm-agent pair`.

Hub creates and manages:

- `/usr/local/bin/farhelm-hub`: the actual program.
- `/usr/local/bin/farhelm-hub.previous`: the single previous program after an update.
- `/etc/farhelm/hub.toml`: the only configuration, normally `0640 root:farhelm-hub`.
- `/var/lib/farhelm/farhelm.db`: persistent database.
- `/etc/systemd/system/farhelm-hub.service`: system service.
- The least-privileged `farhelm-hub` system identity.

Console is embedded in Hub, so there is no external `console/` directory and no installed `farhelmctl`. After editing configuration:

```bash
sudoedit /etc/farhelm/hub.toml
sudo farhelm-hub doctor
sudo farhelm-hub restart
sudo farhelm-hub status
```

For a forgotten password, run `sudo farhelm-hub admin reset-password`; it sets a new password interactively and revokes every browser session.

Hub still listens only on `127.0.0.1:8787`. Use `deploy/hub/Caddyfile.example` to configure Caddy and expose only ports 80/443 publicly.

Update and rollback:

```bash
sudo farhelm-hub update --check
sudo farhelm-hub update
sudo farhelm-hub rollback
```

`upgrade` is a compatibility alias for `update`. Crossing the first version number is denied by default; use `--allow-major` only after explicitly deciding to change it.

Complete removal:

```bash
sudo farhelm-hub uninstall
```

Use `sudo farhelm-hub uninstall --keep-data` to retain TOML and database data. The least-privileged service identity is also retained so saved data keeps a stable UID/GID. The program does not edit the shared Caddy configuration; remove the FarHelm site block and reload Caddy separately.

## Agent

Run as the target regular user on the training host:

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-agent-linux-x86_64 -o farhelm-agent
chmod +x farhelm-agent
./farhelm-agent install
```

V0.8 reuses the Codex already installed and authenticated by this user. It downloads neither Python nor Codex and changes no global model or login configuration. Installation discovers an unambiguous executable; multiple candidates require an explicit choice. The initial acceptance version is Codex 0.153.4, with core protocol compatibility checked against 0.147.0.

If an NVM/npm installation works in your terminal but is missing from the service PATH, configure it from that terminal after installing Agent:

```bash
farhelm-agent codex configure --bin "$(command -v codex)"
farhelm-agent restart
farhelm-agent status
farhelm-agent doctor
```

The configured absolute path and its sibling interpreter directory are used at startup. `status` distinguishes service activity, the Hub connection, and Codex readiness/version. Missing Codex, login failure, or an initialization failure does not stop experiment reporting. If the installed path moves, run `codex configure` again and restart. Offline installation only requires the verified Agent program and an existing local Codex installation.

First generate an eight-digit code under “Servers → Add server”. The installer asks only for the Hub HTTPS URL and pairing code, then stores its dedicated token automatically. For non-interactive installation:

```bash
FARHELM_HUB_URL="https://your-domain" \
FARHELM_PAIRING_CODE="the-eight-digit-code-from-the-Console" \
./farhelm-agent install
```

Agent creates and manages:

- `${XDG_BIN_HOME:-$HOME/.local/bin}/farhelm-agent`: the actual program.
- `farhelm-agent.previous` in the same directory: the single rollback backup.
- `${XDG_CONFIG_HOME:-$HOME/.config}/farhelm/agent.toml`: the only configuration, mode `0600`.
- `${XDG_DATA_HOME:-$HOME/.local/share}/farhelm/`: SQLite state and local connection status.
- `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/farhelm-agent.service`: user service.

If `~/.local/bin` is not yet in the current shell's `PATH`, installation prints the complete command path. Ubuntu normally adds it after the next login; until then, use `~/.local/bin/farhelm-agent` directly.

Common commands:

```bash
farhelm-agent doctor
farhelm-agent status
farhelm-agent restart
journalctl --user -u farhelm-agent -n 50 --no-pager

farhelm-agent update --check
farhelm-agent update
farhelm-agent rollback
farhelm-agent pair
```

`doctor` distinguishes an unreachable Hub, an invalid token, an old shared token, and Worker failure. For credential recovery, create a new pairing code and run `farhelm-agent pair`; no long token needs to be copied or edited.

Every 60 seconds the Agent discovers project candidates from current and archived Codex sessions. Existing sessions appear after “Import all” in the Console. Absolute paths stay local to Agent. Newly discovered projects enable Codex only; experiment automation still requires project-specific log markers.

Continued operation after logout or reboot requires systemd linger for that user. Installation detects and reports this; an administrator only needs to run once:

```bash
loginctl enable-linger your-user
```

Without a systemd user manager, install only the files and run in the foreground:

```bash
./farhelm-agent install --no-service
~/.local/bin/farhelm-agent run --config ~/.config/farhelm/agent.toml
```

Remove everything or keep data:

```bash
farhelm-agent uninstall
farhelm-agent uninstall --keep-data
```

## Migrating from V0.2.0

Hosts already on `V0.3.0` through `V0.8.0` can run `farhelm-hub update` or `farhelm-agent update` directly. `V0.2.0` must first upgrade to `V0.3.0` to migrate the old layout, then upgrade to V0.9.0.

Lowercase legacy `v0.1.0/v0.2.0` releases are outside the formal update sequence. Remove them with their matching old uninstaller before installing V0.9.0.

## Security notes

- The downloaded file is the program; initial installation executes no dynamic remote script.
- V0.3+ updater only downloads versioned assets and verifies the fixed official repository, immutable Release, length, SHA-256, role, and version.
- A new program is fully written on the same filesystem before atomic replacement; failed service health restores previous.
- Configuration and database are not overwritten with the executable; logs go to journald. Existing legacy Python directories remain available for rollback but are never invoked by V0.8.
- The current release permits only typed experiment-observation and Codex session/turn commands. It cannot start or stop training or accept arbitrary cwd/argv/env/shell values; Rust Agent communicates with the installed Codex through local stdio. The Agent opens an outbound WSS connection to `/api/v1/agent/connect`; neither Agent nor Codex accepts inbound network connections. Ensure an upstream reverse proxy permits WebSocket upgrades (the supplied Caddy configuration already does).

## V0.7 data migration

Upgrade Hub before Agent. Each role has one schema migration entry point upgrading its shared database to schema 7 while preserving command and experiment identities. New prompt bodies only pass briefly through Hub memory; unacknowledged legacy bodies remain until durable Agent receipt (expired commands are stored and reported expired without execution). Older database/WAL/backups may contain historical bodies and must remain private.

Schema 7 makes V0.6 and older programs refuse the database. Do not overwrite current execution receipts with an old snapshot to force a downgrade. Retain the current database and use a schema-7-compatible repair build; restore the current binary if binary rollback fails. Restoring an old snapshot can replay completed work and is not a supported rollback path.

V0.7 in-page alerts use the existing SSE connection and durable notification center, with no VAPID or phone permission requirement. Existing Web Push APIs remain compatible; iOS system push is outside this release's acceptance scope.

## V0.7.1 → V0.8.0

Update Hub first, then Agent. The database stays at schema 7; project, session, receipt, experiment and schedule identities are preserved. New Agents use one live channel for reads, commands, receipts and events, without concurrent HTTP polling. Older Agents retain their HTTP compatibility path during migration.

A binary rollback retains the current database and receipts. Never restore an older queue database: it could repeat completed operations. Running work becomes orphaned after restart and requires inspection instead of automatic replay. V0.8 does not downgrade your Codex executable; an older FarHelm installation can reuse its retained Python directory only after rollback verification.

In the browser, verify the permanent system navigation, cached session switching, Markdown, queued sends, interrupts and completion alerts. Conversation caches are memory-only and cleared on logout; at most 20 inactive histories or 24 MiB plus up to 8 MiB of parsed Markdown are retained. Page notifications remain the supported notification experience for this release.

## V0.8.0 → V0.9.0

On the Hub host, run `sudo farhelm-hub update --version V0.9.0` first. Then run `farhelm-agent update --version V0.9.0` as the original user on each Agent host. Check `farhelm-hub status` and `farhelm-agent status`, then refresh the browser. The database stays at schema 7; no re-pairing, project import, or old database restoration is required.

Check device names beside projects, model/permission details, appearance settings, cached session switching, notification pagination/details, and list synchronization after creating/cancelling schedules. Existing server Codex settings continue to apply. Agents without `codex.session_context` must be upgraded before the browser can create sessions with native permissions. Failed submission retries preserve the original operation identity.

Rollback replaces only the executable and retains the current database, configuration, and receipts. Never restore an old queue snapshot. V0.8.0 can still read schema 7, but does not provide the V0.9.0 interface or native permission display features.
