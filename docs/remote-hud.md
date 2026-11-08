# 远端 Magpie 查看

Magpie HUD 可以在 Magpie 所在的 Linux 服务器上运行一个无界面 **HUD Server**，由它读取同机 Magpie 的完整路由、`usage.jsonl`、额度与费用，再通过带查看密钥的 HTTPS 接口提供给本地桌面 HUD 和浏览器。这样即使 Magpie 不在本机，HUD 仍能显示真实模型、账号、Token、费用、额度、实时选路和历史。

## 为什么需要服务端

Magpie 的完整 `/v1/magpie/trace` 只允许服务器本机访问，远程即使持有管理员密钥也无法取得完整账号、会话和组链；远程 `route-events` 字段不全。因此等价查看必须在 Magpie 同机采集，不修改 Magpie 的访问规则。HUD Server 只读 Magpie，不代理模型请求。

## 安装

1. 下载 Linux x86_64 构建包，解压后放到 `/opt/magpie-hud-server/`。CI 使用 Ubuntu 22.04 构建，目标为 glibc 2.35 及更新环境；ARM64 或较旧发行版应在对应环境从源码构建。
2. 创建专用用户与状态目录（日志目录保持只读）：

```bash
sudo useradd --system --home /var/lib/magpie-hud-server --shell /usr/sbin/nologin magpie-hud
sudo install -d -o magpie-hud -g magpie-hud -m 700 /var/lib/magpie-hud-server
sudo install -d -o root -g magpie-hud -m 750 /etc/magpie-hud-server
```

3. 以 `magpie-hud` 用户查看服务账户能否读取 Magpie 日志，并按实际路径编写 `/etc/magpie-hud-server/config.json`（参考 `config.example.json`）。日志和数据目录必须是绝对路径；`listen` 默认只绑定回环。配置文件建议为 `root:magpie-hud`、`0640`。通过组或 ACL 给予服务用户对 Magpie 目录的遍历权限及日志的读取权限，并为日志轮转后的新文件配置默认 ACL；不要把目录改为所有人可读。`callerKeyAliases` 可按独立的 Magpie 调用密钥 ID 配置设备别名，别名只影响显示，不改变统计身份。

4. 创建 HUD 查看密钥。**这不是 Magpie 模型调用密钥**，两者不要混用：

```bash
sudo -u magpie-hud /opt/magpie-hud-server/magpie-hud-server \
  --config /etc/magpie-hud-server/config.json key create --name desktop
```

密钥只显示一次，服务端仅保存校验值。列出和撤销：

```bash
sudo -u magpie-hud /opt/magpie-hud-server/magpie-hud-server --config /etc/magpie-hud-server/config.json key list
sudo -u magpie-hud /opt/magpie-hud-server/magpie-hud-server --config /etc/magpie-hud-server/config.json key revoke <id>
```

撤销后对应桌面客户端和浏览器会话同时失效；需要新密钥时用 `key create`。

5. 安装 systemd 单元并启动：

```bash
sudo cp magpie-hud-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now magpie-hud-server
curl http://127.0.0.1:3435/api/v1/health
```

6. 配置 HTTPS 反向代理（参考 `nginx.conf.example`）。SSE 必须关闭缓冲并延长读超时，否则实时状态会延迟或断开。对外地址写进配置的 `publicUrl`，登录 cookie 才会带 `Secure`。

## 浏览器查看

打开 `https://hud.example.com/`，输入 HUD 查看密钥。密钥通过 POST 换取 HttpOnly、SameSite 的限时会话 cookie，不写入地址栏或 localStorage；退出、过期或服务端撤销后需重新登录。浏览器中置顶、托盘、拖窗等桌面操作不出现，详情页用同源页面。

## 桌面查看

桌面 HUD 设置中选择“远端”，填写主 HUD 地址（如 `https://hud.example.com`）和 HUD 查看密钥。应用用 Rust 发起 HTTPS 请求，密钥保存到 Windows 凭据管理器或 macOS 钥匙串，配置只记录引用；切换回“本地”即恢复原先读取本机 Magpie 的行为。

远端模式不要求本机安装 Magpie 或存在 `usage.jsonl`。首次切换和断线重连会取得完整快照；连接中断时保留最后数据并标注已过期，恢复后自动重新同步。账号用量区的“当前范围 / 全部账号”选择器可切换当前会话使用的账号与服务器提供的全部账号额度，包括当天没有请求的账号。

## 边界

- 一次只选一个权威数据源（本地或一台远端），不把两个网关的日志相加；第一版不做联邦汇总。
- 设备来源依据 Magpie 已记录的转发标签和调用密钥映射，不安装客户端、不做心跳，因此不显示在线/离线。
- 其他设备上的会话标题无法从服务器取得，会显示未命名。
- 没有 session 的合法请求计入管理员总量，并保留在全部用量详情中；不会伪造成会话。总量可能因此大于已命名会话合计。
- 原始发送模型与响应报告模型都保留；已知时以实际响应模型为主，路由分组仍在路径中显示。
