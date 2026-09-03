# FarHelm installation, upgrade, and rollback

[简体中文](README.md) · [English](README.en.md)

Bundles target Ubuntu 24.04 x86_64, or a compatible systemd distribution with glibc 2.39+. The public host needs Caddy or an equivalent HTTPS reverse proxy. A training host needs no root, sudo, Python, or inbound port for presence reporting and side-effect-free probe commands; Python 3.12 is required only for Codex Worker capabilities.

The default entry point is one executable per role. It validates and expands its embedded payload only in an operating-system temporary directory and leaves no extracted directory beside the download. Managed paths are created only when you explicitly run the installer.

## Single-file installation (recommended)

No compilation or GitHub login is required. Download the Hub installer on the public server:

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-hub-linux-x86_64 -o farhelm-hub
chmod +x farhelm-hub
sudo ./farhelm-hub
```

On a training host, download the Agent installer as the regular user and do not use sudo:

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-agent-linux-x86_64 -o farhelm-agent
chmod +x farhelm-agent
./farhelm-agent
```

Agent prompts for the Hub HTTPS URL, Agent ID, and token; token input is hidden. Delete the downloaded installer after success. Advanced users can run `./farhelm-agent --verify` first to validate the embedded bundle without installing.

## 1. Public server

Run the single-file Hub installer:

```bash
sudo ./farhelm-hub
```

The installer generates an admin password and an Agent token, displays them once, and stores them in `/etc/farhelm/hub.env`. The Hub installer manages only:

- `/opt/farhelm-hub/` for `releases/<version>`, atomic `current/previous` links, Console, and management scripts.
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

Expose only ports 80/443, never 8787. Open the HTTPS URL and sign in with the generated admin credentials. The downloaded single-file installer can be deleted after installation.

Later releases need neither uninstall nor manual download. Check first, then upgrade; a failed health check restores previous automatically:

```bash
farhelmctl upgrade --check
sudo farhelmctl upgrade
sudo farhelmctl rollback
```

Crossing the first version number is denied by default. Use `sudo farhelmctl upgrade --allow-major` only after you explicitly decide to change it.

Completely uninstall Hub with:

```bash
sudo /opt/farhelm-hub/uninstall.sh
```

The uninstaller stops the service and removes every FarHelm-specific managed path and system user above. It does not guess how to edit the shared `/etc/caddy/Caddyfile`; remove the FarHelm site block and reload Caddy yourself.

## 2. Training server: unprivileged install

Do not use `sudo`. Run the single file and enter the Agent token printed by the Hub installer when prompted:

```bash
./farhelm-agent
```

Non-interactive automation can still use environment variables. Do not put the token in a command-line argument:

```bash
FARHELM_HUB_URL="https://your-domain.example" \
FARHELM_AGENT_TOKEN="agent-token-from-hub" \
FARHELM_AGENT_ID="gpu-a" \
./farhelm-agent
```

By default, only two FarHelm-specific locations are created:

- `${XDG_DATA_HOME:-~/.local/share}/farhelm-agent/` for persistent configuration/state, `releases/<version>`, atomic `current/previous` links, and the uninstaller; configuration mode is `0600`.
- `${XDG_CONFIG_HOME:-~/.config}/systemd/user/farhelm-agent.service` for the current user's systemd unit.

No system user/group is created, and nothing is written under `/opt`, `/etc`, or `/usr/local`. You may delete the downloaded single-file installer after installation.

Check connectivity:

```bash
systemctl --user status farhelm-agent
journalctl --user -u farhelm-agent -n 50 --no-pager
```

Within one heartbeat interval, Console's Agents page shows the real hostname, Agent ID, version, last heartbeat, and presence.

The `V0.1.0` command channel exposes only `agent.probe` to verify persistence, TTL, and idempotent recovery. It neither reads projects nor runs shell commands. Create a probe with admin authentication, then query the returned `status_url`:

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
./farhelm-agent --no-service

~/.local/share/farhelm-agent/current/run.sh
```

For a temporary foreground run with no installation, use the archive fallback below:

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

Use the installed Agent for later upgrades and rollback, never `sudo`:

```bash
~/.local/share/farhelm-agent/current/bin/farhelm-agent upgrade --check
~/.local/share/farhelm-agent/current/bin/farhelm-agent upgrade
~/.local/share/farhelm-agent/current/bin/farhelm-agent rollback
```

Replace the root path if you used a custom `XDG_DATA_HOME`. Upgrade accepts only immutable uppercase `V*` Releases from the fixed official repository and verifies the GitHub asset length and SHA-256; configuration and databases remain outside release directories.

## Versioned archive fallback

The single-file installer and its embedded archive belong to the same immutable Release. For troubleshooting or package inspection, download the versioned tar.gz files and `SHA256SUMS`, verify them, and run the included `install.sh`:

```bash
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/V0.2.0/farhelm-hub-0.2.0-linux-x86_64.tar.gz
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/V0.2.0/farhelm-agent-0.2.0-linux-x86_64.tar.gz
curl -fLO https://github.com/Xiiiing/FarHelm/releases/download/V0.2.0/SHA256SUMS
grep '\.tar\.gz$' SHA256SUMS | sha256sum -c -
```

## Establishing the new baseline from legacy lowercase releases

Legacy `v0.1.0/v0.2.0` installs use a different layout and remain outside the formal update series. Uppercase `V0.1.x` installations can upgrade directly to current `V0.2.0`; only legacy lowercase installations require cleanup with their old uninstaller first.

## Security notes

- Never expose Hub directly on a public bind address; the application rejects non-loopback binds.
- Never paste `agent.env` or `hub.env` into chat, Git, or public logs.
- Beyond presence, this version executes only a side-effect-free probe. It cannot control training and does not connect to real Codex.
- Formal versions use uppercase `VMAJOR.MINOR.PATCH` tags only. The user alone decides the first number; features and bug fixes increment the second and third numbers respectively.
