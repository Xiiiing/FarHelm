# FarHelm V0.3.0 部署与生命周期

[简体中文](README.md) · [English](README.en.md)

正式程序面向 Ubuntu 24.04 x86_64，或带 systemd、glibc 2.39+ 的兼容系统。Hub 需要 Caddy 或等价 HTTPS 反向代理；Agent 只主动出站，不开放端口，也不需要 sudo。

## Hub

```bash
curl -fL https://github.com/Xiiiing/FarHelm/releases/latest/download/farhelm-hub-linux-x86_64 -o farhelm-hub
chmod +x farhelm-hub
sudo ./farhelm-hub install
```

缺少配置时会询问管理员用户名、管理员密码和 Agent token，密码与 token 隐藏输入。非交互安装使用环境变量：

```bash
sudo env \
  FARHELM_ADMIN_USER="admin" \
  FARHELM_ADMIN_PASSWORD="至少12字符的随机密码" \
  FARHELM_AGENT_TOKEN="至少32字符的随机token" \
  ./farhelm-hub install
```

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

程序依次询问 Hub HTTPS URL、Agent token 和 Agent ID；token 隐藏输入。非交互安装：

```bash
FARHELM_HUB_URL="https://你的域名" \
FARHELM_AGENT_TOKEN="Hub配置中的Agent-token" \
FARHELM_AGENT_ID="gpu-a" \
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
```

编辑 token 或 Hub URL 后运行 `farhelm-agent doctor && farhelm-agent restart`。Hub 与 Agent 的 token 必须一致；任何曾出现在聊天或日志中的 token 都应轮换。

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

大写正式版本 `V0.2.0` 可以直接执行原有 `farhelmctl upgrade` 或 `farhelm-agent upgrade`。V0.3.0 Release 保留一次兼容 tar.gz，旧 updater 验证后会调用新角色程序完成 `.env` 到 TOML、数据库、unit 和稳定二进制迁移。成功后旧 `releases/current/run.sh` 目录会被清理。

旧小写 `v0.1.0/v0.2.0` 不属于正式升级序列，仍需先使用对应旧卸载器清理，再安装 V0.3.0。

## 安全说明

- 下载文件就是程序；首次安装不执行远端动态脚本。
- V0.3+ updater 只下载版本化实际二进制，并验证固定官方仓库、immutable Release、长度、SHA-256、角色和版本。
- 新程序完整写入同一文件系统后才原子替换，服务健康失败自动恢复 previous。
- 配置、数据库和 Worker runtime 不随二进制覆盖；日志进入 journald。
- 当前除在线状态外只执行无副作用 probe，不能启动/停止训练，也不连接真实 Codex。
