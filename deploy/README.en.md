# FarHelm V0.3.0 deployment and lifecycle

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

The uppercase formal `V0.2.0` release can run its existing `farhelmctl upgrade` or `farhelm-agent upgrade`. The V0.3.0 Release retains one compatibility tar.gz; the old updater verifies it, then invokes the new role program to migrate `.env`, database state, the unit, and the stable executable. The old `releases/current/run.sh` tree is removed after success.

Lowercase legacy `v0.1.0/v0.2.0` releases are outside the formal update sequence. Remove them with their matching old uninstaller before installing V0.3.0.

## Security notes

- The downloaded file is the program; initial installation executes no dynamic remote script.
- V0.3+ updater downloads only versioned actual executables and verifies the fixed official repository, immutable Release, length, SHA-256, role, and version.
- A new program is fully written on the same filesystem before atomic replacement; failed service health restores previous.
- Configuration, database, and Worker runtime are not overwritten with the executable; logs go to journald.
- The current release executes only the side-effect-free probe beyond presence reporting; it cannot start/stop training and does not connect to real Codex yet.
