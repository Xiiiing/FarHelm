# FarHelm V0.6.0 部署与生命周期

[简体中文](README.md) · [English](README.en.md)

正式程序面向 Ubuntu 24.04 x86_64，或带 systemd、glibc 2.39+ 的兼容系统。Hub 需要 Caddy 或等价 HTTPS 反向代理；Agent 只主动出站，不开放端口，也不需要 sudo。

## Hub

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-hub-linux-x86_64 -o farhelm-hub
chmod +x farhelm-hub
sudo ./farhelm-hub install
```

缺少配置时只询问管理员用户名和密码。非交互安装使用环境变量：

```bash
sudo env \
  FARHELM_ADMIN_USER="admin" \
  FARHELM_ADMIN_PASSWORD="至少12字符的随机密码" \
  ./farhelm-hub install
```

V0.5.0 可直接执行 `update` 升级；SQLite 会话、配对、项目和调度表会在重启时幂等创建。旧 TOTP 与 Token 字段保留供本机回滚，但 V0.6.0 不再要求 TOTP。V0.3.0 首次跨代安装前仍建议备份：

```bash
sudo cp /var/lib/farhelm/farhelm.db /var/lib/farhelm/farhelm.db.v0.3.bak
sudo cp /etc/farhelm/hub.toml /etc/farhelm/hub.toml.v0.3.bak
sudo ./farhelm-hub install
sudo farhelm-hub doctor
curl -f http://127.0.0.1:8787/api/v1/health
```

已有独立 Token 会自动导入数据库，无需重新配对。仍使用旧共享 Token 的 Agent 会标记“需要配对”；在网页创建 8 位码后执行 `farhelm-agent pair`。

Hub 创建并管理：

- `/usr/local/bin/farhelm-hub`：实际运行程序。
- `/usr/local/bin/farhelm-hub.previous`：仅在升级后存在的上一个程序。
- `/etc/farhelm/hub.toml`：唯一配置，默认 `0640 root:farhelm-hub`。
- `/var/lib/farhelm/farhelm.db`：持久数据库。
- `/etc/systemd/system/farhelm-hub.service`：系统服务。
- `farhelm-hub` 低权限系统身份。

Console 已嵌入 Hub，不存在外置 `console/` 目录，也不再安装 `farhelmctl`。编辑配置后重启：

```bash
sudoedit /etc/farhelm/hub.toml
sudo farhelm-hub doctor
sudo farhelm-hub restart
sudo farhelm-hub status
```

忘记密码时执行 `sudo farhelm-hub admin reset-password`；它会交互设置新密码并撤销全部浏览器会话。

Hub 仍只监听 `127.0.0.1:8787`。参考仓库中的 `deploy/hub/Caddyfile.example` 配置 Caddy；只向公网开放 80/443。

升级和回滚：

```bash
sudo farhelm-hub update --check
sudo farhelm-hub update
sudo farhelm-hub rollback
```

`upgrade` 是 `update` 的兼容别名。默认拒绝跨第一段版本；只有你已明确决定修改第一段时才使用 `--allow-major`。

完全卸载：

```bash
sudo farhelm-hub uninstall
```

若要保留 TOML 和数据库，使用 `sudo farhelm-hub uninstall --keep-data`；为保证保存数据的 UID/GID 归属稳定，低权限服务身份也会保留。程序不会修改共享的 Caddy 主配置，请自行删除 FarHelm 站点块并 reload Caddy。

## Agent

以训练服务器的目标普通用户执行：

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-agent-linux-x86_64 -o farhelm-agent
chmod +x farhelm-agent
./farhelm-agent install
```

Agent 会从同版本不可变 Release 下载独立 Python 3.12/Codex runtime，并校验资产长度和 SHA-256。离线安装时，先传输同版本 runtime 资产，再提供受信任的校验元数据：

```bash
FARHELM_CODEX_RUNTIME_ARCHIVE="$PWD/farhelm-codex-runtime-0.6.0-linux-x86_64.tar.gz" \
FARHELM_CODEX_RUNTIME_SIZE="$(stat -c '%s' farhelm-codex-runtime-0.6.0-linux-x86_64.tar.gz)" \
FARHELM_CODEX_RUNTIME_SHA256="从受信任的SHA256SUMS复制" \
./farhelm-agent install
```

先在网页“服务器 → 添加服务器”生成 8 位码。程序只询问 Hub HTTPS URL 和配对码，独立 Token 自动保存。非交互安装：

```bash
FARHELM_HUB_URL="https://你的域名" \
FARHELM_PAIRING_CODE="网页显示的8位码" \
./farhelm-agent install
```

Agent 创建并管理：

- `${XDG_BIN_HOME:-$HOME/.local/bin}/farhelm-agent`：实际运行程序。
- 同目录的 `farhelm-agent.previous`：唯一回滚备份。
- `${XDG_CONFIG_HOME:-$HOME/.config}/farhelm/agent.toml`：唯一配置，权限 `0600`。
- `${XDG_DATA_HOME:-$HOME/.local/share}/farhelm/`：SQLite 状态与私有 Worker runtime。
- `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/farhelm-agent.service`：用户服务。

如果 `~/.local/bin` 尚未进入当前 shell 的 `PATH`，安装程序会显示完整命令路径；重新登录后 Ubuntu 通常会自动加入，也可以暂时使用 `~/.local/bin/farhelm-agent`。

常用命令：

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

`doctor` 会区分 Hub 不可达、Token 错误、旧共享 Token 和 Worker 故障。凭据恢复时生成新配对码并运行 `farhelm-agent pair`，无需复制或编辑长 Token。

Agent 每 60 秒从 Codex 当前与已归档会话发现项目。网页“一键导入全部”后旧会话自动出现；绝对路径仅保留在 Agent 本地。自动发现的项目默认只启用 Codex，实验自动 prompt 仍需项目专属日志规则。

退出登录或重启后继续运行需要为该用户启用 systemd linger；安装程序会检测并提示。管理员只需执行一次：

```bash
loginctl enable-linger 你的用户名
```

没有用户级 systemd 时可以只安装文件并前台运行：

```bash
./farhelm-agent install --no-service
~/.local/bin/farhelm-agent run --config ~/.config/farhelm/agent.toml
```

完全卸载或保留数据：

```bash
farhelm-agent uninstall
farhelm-agent uninstall --keep-data
```

## 从 V0.2.0 迁移

已安装 `V0.3.0` 至 `V0.5.0` 的主机可以直接执行 `farhelm-hub update` 或 `farhelm-agent update`。`V0.2.0` 必须先升级到 `V0.3.0` 完成旧布局迁移，再升级到 V0.6.0。

旧小写 `v0.1.0/v0.2.0` 不属于正式升级序列，仍需先使用对应旧卸载器清理，再安装 V0.6.0。

## 安全说明

- 下载文件就是程序；首次安装不执行远端动态脚本。
- V0.3+ updater 只下载版本化资产，并验证固定官方仓库、immutable Release、长度、SHA-256、角色和版本；V0.4 对独立 Codex runtime 使用相同校验。
- 新程序完整写入同一文件系统后才原子替换，服务健康失败自动恢复 previous。
- 配置、数据库和 Worker runtime 不随二进制覆盖；日志进入 journald。
- 当前只允许固定类型的实验观察和 Codex session/turn 命令；不能启动/停止训练、传入任意 cwd/argv/env/shell，Codex Worker 只通过 Agent 的本地 stdio 连接真实 SDK。
