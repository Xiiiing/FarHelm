# FarHelm V0.4.1 deployment and lifecycle

[简体中文](README.md) · [English](README.en.md)

Formal programs target Ubuntu 24.04 x86_64, or a compatible system with systemd and glibc 2.39+. Hub needs Caddy or an equivalent HTTPS reverse proxy. Agent is outbound-only, opens no port, and needs no sudo.

## Hub

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-hub-linux-x86_64 -o farhelm-hub
chmod +x farhelm-hub
sudo ./farhelm-hub install
```

When configuration is missing, the program prompts for the admin username, admin password, and Agent token. Password and token input is hidden. For non-interactive installation:

```bash
sudo env \
  FARHELM_ADMIN_USER="admin" \
  FARHELM_ADMIN_PASSWORD="a-random-password-of-at-least-12-characters" \
  FARHELM_AGENT_TOKEN="a-random-token-of-at-least-32-characters" \
  ./farhelm-hub install
```

When upgrading the Hub from V0.3.0, back up its database and configuration, then run `install` once with the V0.4.1 executable. It migrates the plaintext password to Argon2id and prints the TOTP secret and one-time recovery codes to the terminal:

```bash
sudo cp /var/lib/farhelm/farhelm.db /var/lib/farhelm/farhelm.db.v0.3.bak
sudo cp /etc/farhelm/hub.toml /etc/farhelm/hub.toml.v0.3.bak
sudo ./farhelm-hub install
sudo farhelm-hub doctor
curl -f http://127.0.0.1:8787/api/v1/health
```

Before upgrading each Agent, configure a dedicated token for it under `[agents].tokens`. The old `[agents].token` retains only heartbeat and `agent.probe` migration access; it cannot receive Codex commands or upload experiment events.

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

Agent downloads the independent Python 3.12/Codex runtime from the matching immutable Release and verifies its length and SHA-256. For an offline installation, transfer the matching runtime asset first and provide trusted verification metadata:

```bash
FARHELM_CODEX_RUNTIME_ARCHIVE="$PWD/farhelm-codex-runtime-0.4.1-linux-x86_64.tar.gz" \
FARHELM_CODEX_RUNTIME_SIZE="$(stat -c '%s' farhelm-codex-runtime-0.4.1-linux-x86_64.tar.gz)" \
FARHELM_CODEX_RUNTIME_SHA256="copy-from-trusted-SHA256SUMS" \
./farhelm-agent install
```

The program prompts for the Hub HTTPS URL, Agent token, and Agent ID; token input is hidden. For non-interactive installation:

```bash
FARHELM_HUB_URL="https://your-domain" \
FARHELM_AGENT_TOKEN="the-Agent-token-from-Hub-config" \
FARHELM_AGENT_ID="gpu-a" \
./farhelm-agent install
```

Agent creates and manages:

- `${XDG_BIN_HOME:-$HOME/.local/bin}/farhelm-agent`: the actual program.
- `farhelm-agent.previous` in the same directory: the single rollback backup.
- `${XDG_CONFIG_HOME:-$HOME/.config}/farhelm/agent.toml`: the only configuration, mode `0600`.
- `${XDG_DATA_HOME:-$HOME/.local/share}/farhelm/`: SQLite state and private Worker runtime.
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
```

After editing the token or Hub URL, run `farhelm-agent doctor && farhelm-agent restart`. Hub and Agent tokens must match. Rotate any token that has appeared in chat or logs.

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

Hosts already on `V0.3.0` or `V0.4.0` can run `farhelm-hub update` or `farhelm-agent update` directly. `V0.2.0` must first upgrade to `V0.3.0`, which retains the compatibility archive needed to migrate `.env`, database state, the unit, and the stable executable, and can then upgrade to V0.4.1. V0.4.1 does not republish compatibility tarballs.

Lowercase legacy `v0.1.0/v0.2.0` releases are outside the formal update sequence. Remove them with their matching old uninstaller before installing V0.4.1.

## Security notes

- The downloaded file is the program; initial installation executes no dynamic remote script.
- V0.3+ updater only downloads versioned assets and verifies the fixed official repository, immutable Release, length, SHA-256, role, and version; V0.4 applies the same checks to the independent Codex runtime.
- A new program is fully written on the same filesystem before atomic replacement; failed service health restores previous.
- Configuration, database, and Worker runtime are not overwritten with the executable; logs go to journald.
- The current release permits only typed experiment-observation and Codex session/turn commands. It cannot start or stop training or accept arbitrary cwd/argv/env/shell values; the Codex Worker connects to the real SDK only through the Agent's local stdio channel.
