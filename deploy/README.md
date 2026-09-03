# FarHelm 最小部署

[简体中文](README.md) · [English](README.en.md)

发布包面向 Ubuntu 24.04 x86_64（或带 systemd、glibc 2.39+ 的兼容发行版）。公网服务器需要 Caddy（或等价 HTTPS 反向代理）。训练服务器只上报在线心跳时不需要 root、sudo 或 Python，也不需要开放入站端口；Codex Worker 能力才需要 Python 3.12。

下载并解压压缩包只会产生一个同名目录，不会自动安装任何文件。只有执行 `install.sh` 后才会创建下文列出的受管路径。

## 1. 公网服务器

上传并解压 `farhelm-hub-0.1.0-linux-x86_64.tar.gz`：

```bash
tar -xzf farhelm-hub-0.1.0-linux-x86_64.tar.gz
cd farhelm-hub-0.1.0-linux-x86_64
sudo ./install.sh
```

安装器会生成管理员密码和 Agent token，只在终端显示一次，同时保存到 `/etc/farhelm/hub.env`。Hub 安装器只管理：

- `/opt/farhelm-hub/`：二进制、Console 和卸载器。
- `/etc/farhelm/hub.env` 与 `/etc/farhelm/Caddyfile.example`：配置和代理示例。
- `/etc/systemd/system/farhelm-hub.service`：系统服务。
- `/usr/local/bin/farhelmctl`：健康检查 CLI。
- `farhelm-hub` 系统用户；不创建 home 目录。

随后编辑包中的 `Caddyfile.example`，把域名改成自己的域名并合并到 `/etc/caddy/Caddyfile`：

```bash
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
curl https://你的域名/api/v1/health
```

确认云安全组/防火墙只开放 80/443，不开放 8787。浏览器打开 HTTPS 域名，使用安装器输出的管理员凭据登录。安装成功后可以删除上传并解压出来的目录，运行文件已经复制到受管路径。

完全卸载 Hub：

```bash
sudo /opt/farhelm-hub/uninstall.sh
```

卸载器会停止服务并删除以上全部 FarHelm 专属路径和系统用户。它不会猜测修改共享的 `/etc/caddy/Caddyfile`；请手动删除 FarHelm 站点块并 reload Caddy。

## 2. 训练服务器：普通用户安装

不要使用 `sudo`。使用 Hub 安装器生成的 Agent token：

```bash
tar -xzf farhelm-agent-0.1.0-linux-x86_64.tar.gz
cd farhelm-agent-0.1.0-linux-x86_64

FARHELM_HUB_URL="https://你的域名" \
FARHELM_AGENT_TOKEN="Hub输出的Agent-token" \
FARHELM_AGENT_ID="gpu-a" \
./install.sh
```

默认只创建两个 FarHelm 专属位置：

- `${XDG_DATA_HOME:-~/.local/share}/farhelm-agent/`：二进制、Worker、配置和卸载器；配置权限为 `0600`。
- `${XDG_CONFIG_HOME:-~/.config}/systemd/user/farhelm-agent.service`：当前用户的 systemd 服务。

它不会创建系统用户/组，不写 `/opt`、`/etc` 或 `/usr/local`。安装成功后可以删除下载的压缩包和解压目录。

检查连接：

```bash
systemctl --user status farhelm-agent
journalctl --user -u farhelm-agent -n 50 --no-pager
```

约一个心跳周期内，Console 的“服务器”页面会出现真实主机名、Agent ID、版本、最后心跳和在线状态。

用户服务可以立即运行，但退出登录或重启后持续运行需要服务器为该用户启用 systemd linger。安装器会检测并提示；若未启用，需要管理员执行一次：

```bash
loginctl enable-linger 你的用户名
```

如果用户级 systemd 不可用，可以只安装而不创建服务：

```bash
FARHELM_NO_SERVICE=1 \
FARHELM_HUB_URL="https://你的域名" \
FARHELM_AGENT_TOKEN="Hub输出的Agent-token" \
FARHELM_AGENT_ID="gpu-a" \
./install.sh

~/.local/share/farhelm-agent/run.sh
```

也可以完全不安装，直接在解压目录以前台方式运行：

```bash
FARHELM_HUB_URL="https://你的域名" \
FARHELM_AGENT_TOKEN="Hub输出的Agent-token" \
FARHELM_AGENT_ID="gpu-a" \
./bin/farhelm-agent run
```

此时删除解压目录即可，不会留下 FarHelm 文件。用户安装模式的完整卸载命令为：

```bash
${XDG_DATA_HOME:-$HOME/.local/share}/farhelm-agent/uninstall.sh
```

## 安全说明

- 不得让 Hub 直接监听公网地址；应用会拒绝非 loopback bind。
- 不要在聊天、Git 或公开日志中粘贴任何 `agent.env` 或 `hub.env`。
- 当前版本只上报在线状态，不能启动/停止训练，也不连接真实 Codex。
