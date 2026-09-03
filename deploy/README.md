# FarHelm 最小部署

[简体中文](README.md) · [English](README.en.md)

发布包面向 Ubuntu 24.04 x86_64（或带 systemd、glibc 2.39+ 的兼容发行版）。公网服务器需要 Caddy（或等价 HTTPS 反向代理）；训练服务器需要 Python 3.12。训练服务器无需开放入站端口。

## 1. 公网服务器

上传并解压 `farhelm-hub-0.1.0-linux-x86_64.tar.gz`：

```bash
tar -xzf farhelm-hub-0.1.0-linux-x86_64.tar.gz
cd farhelm-hub-0.1.0-linux-x86_64
sudo ./install.sh
```

安装器会生成管理员密码和 Agent token，只在终端显示一次，同时保存到 `/etc/farhelm/hub.env`。随后编辑包中的 `Caddyfile.example`，把域名改成自己的域名并合并到 `/etc/caddy/Caddyfile`：

```bash
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
curl https://你的域名/api/v1/health
```

确认云安全组/防火墙只开放 80/443，不开放 8787。浏览器打开 HTTPS 域名，使用安装器输出的管理员凭据登录。

## 2. 训练服务器

使用 Hub 安装器生成的 Agent token：

```bash
tar -xzf farhelm-agent-0.1.0-linux-x86_64.tar.gz
cd farhelm-agent-0.1.0-linux-x86_64
sudo FARHELM_RUN_USER="$USER" \
  FARHELM_HUB_URL="https://你的域名" \
  FARHELM_AGENT_TOKEN="Hub输出的Agent-token" \
  FARHELM_AGENT_ID="gpu-a" \
  ./install.sh
```

检查连接：

```bash
systemctl status farhelm-agent
journalctl -u farhelm-agent -n 50 --no-pager
```

约一个心跳周期内，Console 的“服务器”页面会出现真实主机名、Agent ID、版本、最后心跳和在线状态。

## 安全说明

- 不得让 Hub 直接监听公网地址；应用会拒绝非 loopback bind。
- 不要在聊天、Git 或公开日志中粘贴 `/etc/farhelm/*.env`。
- 修改配置后运行 `sudo systemctl restart farhelm-hub` 或 `farhelm-agent`。
- 当前版本只上报在线状态，不能启动/停止训练，也不连接真实 Codex。
