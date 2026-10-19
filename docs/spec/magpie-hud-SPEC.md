# MagpieHUD — Magpie 路由监视悬浮窗 — 实现说明（Tauri + WebView）

> 给实现 Agent 用。目标：用**外部程序**实时显示 Magpie 的路由结果，不改 Magpie 本体。
> 参考截图：主界面「Agent → Magpie → 多账号分流」动画 + 底部路由判决两行文案。

---

## 1. 要做成什么

做一个 Windows 常驻**桌面悬浮窗**，在 Claude Desktop / 其它 Agent 经 Magpie 发请求时：

1. **上方**：简化版路由流向图（不是 1:1 复刻 Magpie 全页，但观感接近）
2. **下方**：固定 **2 行**最新路由说明，**每行带本地时间**
3. **实时**：Magpie 写完一条路由记录后，窗口应在约 **300ms 内**刷新（目标 &lt;500ms）

产品名：**MagpieHUD**（仓库/crate：`magpie-hud`）（可改）。独立 exe，与 Magpie、rs_agent_router 并列运行。

---

## 0. 项目命名

- **显示名**：MagpieHUD
- **仓库 / crate / exe**：`magpie-hud`
- **含义**：Magpie 路由结果的桌面 HUD（抬头显示），外挂监视器，非 Magpie 本体

---

## 2. 技术栈（已定）

| 层 | 选型 | 说明 |
| --- | --- | --- |
| 壳 | **Tauri 2** | 小体积、系统 WebView、易做置顶无边框窗 |
| 前端 | HTML/CSS/JS 或 React/Svelte（任选一种，保持简单） | 画节点连线 + 两行文本 |
| 后端 | Rust（Tauri command / 后台任务） | 监视文件、解析、推事件到前端 |
| 目标平台 | **Windows 10/11**（用户主环境 HF-459） | 先只保证 Win；路径按用户主目录解析 |

不要用 Electron。不要嵌入 Magpie 进程。

---

## 3. 数据源（只读）

### 3.1 主数据源（必须）

文件：`%USERPROFILE%\.config\magpie\usage.jsonl`

- 每行一个 JSON 对象，请求结束时追加
- 用文件系统监视（`notify` crate 或 Win `ReadDirectoryChanges`）监听 `Write`/`Rename`
- 触发后：**只读文件尾部**（最后 64KB 或最后 N 行），不要全量加载
- 解析失败的行跳过，不要崩

### 3.2 已知字段（以实机为准，缺字段则降级显示）

从现有样本可见字段包括（不完整，实现时以实际一行 `serde_json::Value` 容错）：

- `t`：时间戳，如 `2026-10-19T20:33:19.5306096+08:00` → 界面显示 `20:33:19`（用户时区 Asia/Shanghai）
- `agent`：如 `claude-desktop`
- `providerAccount` / `host`：账号邮箱
- `model` / `served` / `req`：请求模型与实际服务模型（如 `group/smart-split` → `grok-4.7-build-fast@500k`）
- `effort`：如 `low`
- `ms`、`ttft_ms`：总耗时、首字耗时（毫秒）
- `in` / `out` / `cache_read` / `cache_write`：token
- `status`、`route_id`、`session`

### 3.3 可选增强（二期，不要阻塞 MVP）

- HTTP：`http://127.0.0.1:3425/v1/magpie/usage` 或 `/v1/magpie/usage/requests`、`/v1/magpie/route`（可能需 gateway key；MVP **不要依赖**）
- Magpie 主窗口 UI：**禁止**抓窗、禁止注入、禁止读 Magpie 内存

### 3.4 文案「不参与 / 本轮判断选出」

Magpie 路由页底部那种完整中文叙述，jsonl **不一定**原样提供。

MVP 规则：

- **第 1 行（命中）**：根据最新一条成功记录拼人话，例如：  
  `20:33:19  252964683@qq.com (grok-4.7-build-fast@500k) 以 low 用 3.8s 完成 · 190.0k token · 3.0s 后首响 · 缓存命中率 xx%。req=group/smart-split`
- **第 2 行（对照/说明）**：若同一次路由能推断「Agent 要的强度被改过」则写；否则写上一条不同账号的跳过原因，或写 `—` / 省略。  
  若短期内无法从 jsonl 得到「不参与」原因：**第 2 行允许先显示「候补/其它账号未选中」+ 时间**，并在 README 标明「完整排除原因待二期对接路由 API」。

缓存命中率估算（若有 cache_read）：  
`cache_hit% ≈ cache_read / (in + cache_read) * 100`（注明为估算）。

Token 显示：`(in+out+cache_read)/1000` 显示为 `xxx.xk token`（与 Magpie 观感接近即可）。

---

## 4. UI 规格

### 4.1 窗口行为

- 无边框、半透明深色底、**Always on Top**
- 可拖动；记住上次位置（本地配置文件）
- **多显示器**：按窗口所在屏工作区布局，**不得超出屏幕、不得盖住任务栏**（参考用户 AR 悬浮窗规则：下半屏内容向上展开）
- 托盘图标：显示/隐藏窗口；退出
- 失焦**不要**自动关闭（与 Magpie tray panel 不同；这是监视器）
- 默认尺寸建议：宽 720–900px，高 220–320px（可调）；支持角落缩放可选

### 4.2 上方：简化分流图（MVP 必须有，但允许简化）

最少节点：

1. 左侧：`agent`（如 Claude Desktop）+ 副标题 `req`（如 `group/smart-split`）
2. 中间：`magpie` + `127.0.0.1:3425`（端口可配置，默认 3425）
3. 右侧：当前命中账号一行（邮箱 + 模型），用**高亮色连线**（参考截图粉/洋红）

连线：贝塞尔曲线即可（SVG 或 canvas）。  
每次新记录：短暂高亮动画（200–400ms），然后保持命中态。

**不要**在 MVP 画 Magpie 里全部候补账号列表；最多右侧额外灰显 1–2 个「未选中」占位（可选）。

### 4.3 下方：固定 2 行日志

- 永远只显示 **2 行**（更新时上移/替换，不要滚动历史墙）
- 每行**左侧或行首**标注时间 `HH:mm:ss`
- 字号清晰可读；长文本单行省略号或限高两行内自动换行（优先单行省略）
- 样式贴近 Magpie 深色主题即可，无需像素级一致

### 4.4 空态 / 错误态

- 尚无记录：`等待 Magpie 路由写入…` + 显示监视的文件路径
- 文件不存在：提示「未检测到 Magpie usage.jsonl，请确认 Magpie 已运行过」
- 解析错误：托盘或角标轻提示，界面保留上一帧有效数据

---

## 5. 实时管道（Rust 侧）

建议流程：

1. 启动 → 解析 `usage.jsonl` 路径 → 读尾部得到「当前最新 1–2 条」→ 推给前端
2. `notify` 监视该文件（及所在目录，兼容编辑器式替换写入）
3. debounce **50–150ms**，合并连写
4. 读尾 → 解析 → 若 `route_id`/`t` 与上次相同则跳过
5. `emit` 到 WebView：`route-update` 事件，payload 为结构化 JSON（不要只丢原始行给前端拼全部逻辑；前端可再格式化文案）

配置文件（例 `%APPDATA%\magpie-hud\config.json`）：

- `usage_jsonl_path`（可空=默认）
- `magpie_host` 显示用
- `window` 位置尺寸
- `always_on_top`（默认 true）

---

## 6. 明确不做（Out of scope）

- 不修改 Magpie 源码 / 不 PR 上游（除非后期单独任务）
- 不复刻完整 Routing 页（多账户拖拽座位、分组编辑器、意图规则编辑）
- 不代理、不改写请求
- 不做跨机器远程监视（MVP）
- 不集成进 rs_agent_router（可同机并存；不要把代码揉进 AR 仓库，除非用户改口）

---

## 7. 验收标准

1. Magpie 经 Claude Desktop 走 `group/smart-split` 完成一次回答后，悬浮窗在 **500ms 内**更新命中账号与底部文案
2. 底栏始终 ≤2 行，且含 `HH:mm:ss`
3. 窗口置顶、可拖、重启后位置恢复
4. 三屏/不同 DPI 下不跑出工作区、不长期压住任务栏
5. Magpie 未开或文件缺失时有明确空态，进程不崩
6. README：如何运行、`cargo tauri build`、默认路径、与 Magpie 的关系

---

## 8. 建议目录与里程碑

仓库建议新建：`magpie-hud`（Rust workspace + `src-tauri` + `ui`）

| 里程碑 | 内容 |
| --- | --- |
| M1 | Tauri 无边框置顶窗 + 读 jsonl 尾 + 底 2 行带时间 |
| M2 | 三节点 SVG 流向 + 命中高亮动画 + 托盘 |
| M3 | 多屏安全区、配置、安装/开机可选、文案打磨 |

优先交付 **M1+M2**。

---

## 9. 给实现者的约束

- 默认用中文 UI 文案
- 密钥/账号仅显示界面已有信息，**不要**把 `logins.json` 或 API key 读进本程序
- 所有路径用用户主目录，不要写死 `C:\Users\kaizhong_liu\...`
- 提交信息清晰；附一张运行截图到 README

---

## 10. 参考

- Magpie 配置目录：`~/.config/magpie/`
- Magpie 网关本机：`http://127.0.0.1:3425`（用户当前进程）
- 用户已有悬浮窗经验项目：`rs_agent_router`（多屏/任务栏规则可对照，**代码勿强行合并**）
- Magpie 官方是路由页内嵌动画；本项目是**外挂监视器**，视觉「神似」即可
