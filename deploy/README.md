# FarHelm V0.11.0 部署与生命周期

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

V0.5.0 可直接执行 `update` 升级；SQLite 会话、配对、项目和调度表会在重启时幂等创建。旧 TOTP 与 Token 字段保留供本机回滚，但 V0.8.0 不再要求 TOTP。V0.3.0 首次跨代安装前仍建议备份：

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

V0.8 复用当前用户已安装、登录的 Codex，不下载 Python 或 Codex，不修改全局模型与登录配置。安装时自动发现唯一可用程序；存在多个候选时需要明确选择。首个验收版本是 Codex 0.153.4，核心协议兼容检查覆盖 0.147.0。

如果终端可以使用 NVM/npm 安装的 Codex，但服务 PATH 找不到，请在该终端安装 Agent 后执行：

```bash
farhelm-agent codex configure --bin "$(command -v codex)"
farhelm-agent restart
farhelm-agent status
farhelm-agent doctor
```

服务启动固定使用配置的绝对路径及同目录解释器。`status` 分别展示服务运行、Hub 连接与 Codex 就绪状态及版本。未安装 Codex、需要登录或初始化失败不影响实验上报。安装路径变化后重新运行 `codex configure` 并重启。离线安装只需已校验的 Agent 程序及已有本机 Codex。

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
- `${XDG_DATA_HOME:-$HOME/.local/share}/farhelm/`：SQLite 状态与本地连接状态。
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

已安装 `V0.3.0` 至 `V0.9.0` 的主机可以直接执行 `farhelm-hub update` 或 `farhelm-agent update`。`V0.2.0` 必须先升级到 `V0.3.0` 完成旧布局迁移，再升级到 V0.10.0。

旧小写 `v0.1.0/v0.2.0` 不属于正式升级序列，仍需先使用对应旧卸载器清理，再安装 V0.10.0。

## 安全说明

- 下载文件就是程序；首次安装不执行远端动态脚本。
- V0.3+ updater 只下载版本化资产，并验证固定官方仓库、immutable Release、长度、SHA-256、角色和版本。
- 新程序完整写入同一文件系统后才原子替换，服务健康失败自动恢复 previous。
- 配置、数据库不随二进制覆盖；日志进入 journald。已有 Python 目录保留供回退使用，但 V0.8 不再调用。
- 当前只允许固定类型的实验观察和 Codex session/turn 命令；不能启动/停止训练、传入任意 cwd/argv/env/shell，Rust Agent 只通过本地 stdio 连接已安装的 Codex。Agent 主动建立到 `/api/v1/agent/connect` 的出站 WSS，Agent 和 Codex 不接受入站网络连接。上游反向代理需要允许 WebSocket 升级（提供的 Caddy 配置已经支持）。

## V0.7 数据升级

先升级 Hub，再升级 Agent。共享数据库只通过角色迁移入口升级到 schema 7；保留现有命令和实验身份。新指令正文只短暂经过 Hub 内存，旧版本未确认交付的正文保留到 Agent 持久接收（已过期指令仅接收并报告过期，不执行）。升级前的数据库/WAL/备份可能仍含历史正文，须按私有数据保存。

schema 7 会让 V0.6 及更早程序拒绝打开数据库，因此不能用旧数据库快照覆盖当前执行记录来强行降级。保留当前数据库并使用兼容 schema 7 的修复版；二进制回滚失败时恢复当前程序。恢复旧快照可能重放已执行操作，不属于支持的回滚路径。

V0.7 的页面通知通过现有 SSE 和持久通知中心工作，无需 VAPID 或手机通知权限。已有 Web Push 接口保留兼容；iOS 系统推送不在本版验收范围。

## V0.7.1 → V0.8.0

先更新 Hub，再更新 Agent。数据库保持 schema 7，保留项目、会话、收据、实验和调度身份。新 Agent 通过一个长连接传输读取、命令、收据和事件，不同时进行 HTTP 轮询；旧 Agent 在迁移期间保留 HTTP 兼容链路。

二进制回退始终保留当前数据库和收据。不能恢复旧队列数据库，否则可能重复执行已完成操作。重启中的运行任务标记为 orphaned，需检查结果，不自动重放。V0.8 不会降级用户 Codex；旧 FarHelm 只有在回退验证后才可复用保留的 Python 目录。

在浏览器中检查始终可见的系统导航、会话缓存切换、Markdown、排队发送、中断和完成通知。对话缓存仅在内存中，退出登录时清除；非活动历史最多 20 个或 24 MiB，另保留最多 8 MiB 的 Markdown 解析缓存。本版继续使用页面通知。

## V0.8.0 → V0.9.0

在 Hub 主机先执行 `sudo farhelm-hub update --version V0.9.0`，再在每台 Agent 主机以原用户执行 `farhelm-agent update --version V0.9.0`。分别运行 `farhelm-hub status` 与 `farhelm-agent status`，然后刷新浏览器。数据库保持 schema 7，不需要重新配对、导入项目或恢复旧数据库。

检查项目旁的设备名称、模型/权限详情、主题设置、缓存会话切换、通知分页与详情，以及创建/取消定时任务后的列表同步。服务器原有 Codex 配置继续生效；旧 Agent 缺少 `codex.session_context` 时需升级后才能通过网页新建原生权限会话。失败重试保留原操作身份。

回退只替换二进制并保留当前数据库、配置和收据；不要恢复旧队列快照。V0.8.0 仍可读取 schema 7，回退后不提供 V0.9.0 的界面与原生权限展示能力。

## V0.9.0 → V0.10.0

先在 Hub 主机执行 `sudo farhelm-hub update --version V0.10.0`，再以原用户在 Agent 主机执行 `farhelm-agent update --version V0.10.0`，最后刷新网页。新模型选择与原生会话菜单需要两端都完成升级；项目、会话、收据和 schema 7 保持兼容，不需要重新配对。

打开会话，点击输入区右下角的模型名称，选择本机 Codex 提供的模型与推理强度，再发送一条指令。选择用于下一轮排队及后续对话，当前运行轮次的补充指令不会更换模型；权限和全局配置保持原样。定时任务沿用执行时的会话设置。

如果在原生客户端找不到会话，打开会话菜单的“在原生 Codex 中继续”核对保存状态和 ID，并在同服务器、同用户的终端执行复制的 `codex resume` 命令。新空会话先发送消息，旧通用名称可通过“重命名会话”统一；桌面端可能需要刷新列表或重新打开项目。回退仍只替换二进制并保留当前数据库；V0.9.0 不提供新的模型选择和原生会话菜单。

若 FarHelm 仍占用原生写入连接，先在该弹窗点击“释放连接后继续”。存在活动任务、后台终端或未保存会话时会拒绝交接，处理完后可重试；不会终止它们。原生客户端用完后关闭会话连接，再回到网页发送。已有定时任务仍按计划执行，不会因交接自动暂停。

## V0.11.0 升级

先升级 Hub，再以原用户升级各台 Agent；升级前结束正在执行的会话并备份配置和数据库。各角色唯一迁移入口将 schema 7 升级为 8，保留项目、会话、授权、收据和执行身份，无需重新配对：

```bash
# Hub
sudo farhelm-hub update --version V0.11.0
sudo farhelm-hub status

# 各台 Agent，不使用 sudo
farhelm-agent update --version V0.11.0
farhelm-agent status
```

更新后重新打开网页，在“项目管理”中添加项目和选择展示范围。目录根授权通过 Agent 本地 `project roots add/list/remove` 管理；原生归档范围检查需要 Codex 0.153.4 或以上版本。

V0.10.0 及更早程序拒绝 schema 8；回退需要兼容 schema 8 的修复程序，不要用旧数据库快照覆盖已完成操作的收据。新目录和生命周期命令只发给声明相应能力的 Agent，未升级服务器会显示升级提示。
