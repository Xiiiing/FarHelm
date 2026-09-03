# FarHelm minimal deployment

[简体中文](README.md) · [English](README.en.md)

Bundles target Ubuntu 24.04 x86_64, or a compatible systemd distribution with glibc 2.39+. The public host needs Caddy or an equivalent HTTPS reverse proxy. A training host needs no root, sudo, Python, or inbound port for presence reporting and side-effect-free probe commands; Python 3.12 is required only for Codex Worker capabilities.

Downloading and extracting a bundle creates only one same-named directory and does not install anything. Managed paths are created only after you run `install.sh`.

## Download from GitHub

No compilation or GitHub login is required. Download the Hub bundle on the public server, the Agent bundle on the training server, and the checksum file on both:

```bash
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/v0.2.0/farhelm-hub-0.2.0-linux-x86_64.tar.gz
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/v0.2.0/farhelm-agent-0.2.0-linux-x86_64.tar.gz
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/v0.2.0/SHA256SUMS
sha256sum -c SHA256SUMS
```

If a host downloads only one bundle, verify that file with:

```bash
grep 'farhelm-hub-' SHA256SUMS | sha256sum -c -
# or
grep 'farhelm-agent-' SHA256SUMS | sha256sum -c -
```

## 1. Public server

Upload and extract `farhelm-hub-0.2.0-linux-x86_64.tar.gz`:

```bash
tar -xzf farhelm-hub-0.2.0-linux-x86_64.tar.gz
cd farhelm-hub-0.2.0-linux-x86_64
sudo ./install.sh
```

The installer generates an admin password and an Agent token, displays them once, and stores them in `/etc/farhelm/hub.env`. The Hub installer manages only:

- `/opt/farhelm-hub/` for binaries, Console, and the uninstaller.
- `/etc/farhelm/hub.env` and `/etc/farhelm/Caddyfile.example` for configuration and the proxy example.
- `/etc/systemd/system/farhelm-hub.service` for the system service.
- `/usr/local/bin/farhelmctl` for the health-check CLI.
- `/var/lib/farhelm-hub/` for the durable command database, removed by uninstall.
- The `farhelm-hub` system user, without a home directory.

Replace the hostname in `Caddyfile.example`, merge the site block into `/etc/caddy/Caddyfile`, then run:

```bash
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
curl https://your-domain.example/api/v1/health
```

Expose only ports 80/443, never 8787. Open the HTTPS URL and sign in with the generated admin credentials. The uploaded archive and extracted directory can be deleted after installation.

Completely uninstall Hub with:

```bash
sudo /opt/farhelm-hub/uninstall.sh
```

The uninstaller stops the service and removes every FarHelm-specific managed path and system user above. It does not guess how to edit the shared `/etc/caddy/Caddyfile`; remove the FarHelm site block and reload Caddy yourself.

## 2. Training server: unprivileged install

Do not use `sudo`. Use the Agent token printed by the Hub installer:

```bash
tar -xzf farhelm-agent-0.2.0-linux-x86_64.tar.gz
cd farhelm-agent-0.2.0-linux-x86_64

FARHELM_HUB_URL="https://your-domain.example" \
FARHELM_AGENT_TOKEN="agent-token-from-hub" \
FARHELM_AGENT_ID="gpu-a" \
./install.sh
```

By default, only two FarHelm-specific locations are created:

- `${XDG_DATA_HOME:-~/.local/share}/farhelm-agent/` for the binary, Worker, configuration, command-state database, and uninstaller; configuration mode is `0600`.
- `${XDG_CONFIG_HOME:-~/.config}/systemd/user/farhelm-agent.service` for the current user's systemd unit.

No system user/group is created, and nothing is written under `/opt`, `/etc`, or `/usr/local`. You may delete the downloaded archive and extracted directory after installation.

Check connectivity:

```bash
systemctl --user status farhelm-agent
journalctl --user -u farhelm-agent -n 50 --no-pager
```

Within one heartbeat interval, Console's Agents page shows the real hostname, Agent ID, version, last heartbeat, and presence.

The `0.2.0` command channel exposes only `agent.probe` to verify persistence, TTL, and idempotent recovery. It neither reads projects nor runs shell commands. Create a probe with admin authentication, then query the returned `status_url`:

```bash
curl --user admin \
  --header 'Content-Type: application/json' \
  --data '{"idempotency_key":"manual-probe-0001","ttl_secs":60}' \
  https://your-domain.example/api/v1/agents/gpu-a/probe
```

The user service starts immediately, but continued operation after logout or reboot requires systemd linger to be enabled for the account. The installer detects and reports this. If disabled, an administrator must run once:

```bash
loginctl enable-linger your-user
```

If the systemd user manager is unavailable, install without a service and run in the foreground:

```bash
FARHELM_NO_SERVICE=1 \
FARHELM_HUB_URL="https://your-domain.example" \
FARHELM_AGENT_TOKEN="agent-token-from-hub" \
FARHELM_AGENT_ID="gpu-a" \
./install.sh

~/.local/share/farhelm-agent/run.sh
```

You can also skip installation entirely and run directly from the extracted directory:

```bash
FARHELM_HUB_URL="https://your-domain.example" \
FARHELM_AGENT_TOKEN="agent-token-from-hub" \
FARHELM_AGENT_ID="gpu-a" \
./bin/farhelm-agent run
```

Deleting the extracted directory then leaves no FarHelm files. To completely remove a user installation, run:

```bash
${XDG_DATA_HOME:-$HOME/.local/share}/farhelm-agent/uninstall.sh
```

## Security notes

- Never expose Hub directly on a public bind address; the application rejects non-loopback binds.
- Never paste `agent.env` or `hub.env` into chat, Git, or public logs.
- Beyond presence, this version executes only a side-effect-free probe. It cannot control training and does not connect to real Codex.
