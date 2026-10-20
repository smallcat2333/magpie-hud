# magpie-hud

使用 **Tauri 2 + 系统 WebView** 构建 Windows 独立悬浮窗，只读 Magpie 的 `usage.jsonl`，显示简化分流图与固定两行带本地时间的路由说明。不修改 Magpie、不读取密钥、不集成进 rs_agent_router；MVP 不依赖 HTTP API。

**当前状态：会话路由和当日统计已接入实际 Tauri 窗口。** 默认窗口 1120×400，最左侧显示当日 Token、费用记录及 Tks，右侧扩大为模型图标和两行文字的会话路由块；底部仍固定两行。完整需求见 [SPEC](docs/spec/magpie-route-float-SPEC.md)，2026-10-20 的会话/统计增量见 [M2 档案](docs/feat/Feat_261019_2_分流图与托盘.md)。原始 [HTML 原型](docs/ui/magpie-hud.html) 保留为早期设计参考，实际程序使用 `ui/index.html`、`ui/hud.css` 和 `ui/hud.js`。

## 运行

需要 Rust stable、Windows 10/11 WebView2 Runtime 和 **Tauri CLI 2**。本机已于 2026-10-20 确认安装 `tauri-cli 2.12.3`，替换原来的 1.6.1。

首次安装 CLI：

```bash
cargo install tauri-cli --version "^2.0.0" --locked
```

在项目根目录启动开发窗口：

```bash
cargo tauri dev
```

构建发布包（本次尚未执行）：

```bash
cargo tauri build
```

运行 Rust 单元测试：

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

默认读取 `%USERPROFILE%\.config\magpie\usage.jsonl`，配置位于 `%APPDATA%\magpie-hud\config.json`。配置字段为 `usageJsonlPath`、`magpieHost`、`window` 和 `alwaysOnTop`；地址只用于显示，不发起 HTTP 请求。置顶按钮控制窗口置顶，隐藏按钮把窗口隐藏到托盘；托盘菜单提供显示、隐藏和退出。

日志在请求结束后写入，≤500ms 的刷新目标从完整记录落盘开始计算。启动以 64KB 分块流式汇总本地当日记录；运行中只读新增字节，半行留到完整后再处理。`kind=routing` 的内部分类调用不计入统计，重复行不重复累计。

## 会话、状态与当日统计

- 右上“最近会话”显示最近三个会话，选具体会话后，路由、底部说明及左侧当日统计同步切换。日志没有会话名称，只展示 `session` 短号，完整 ID 用于分组并保留在提示里；缺失 ID 明确显示“未标记会话”。
- 右侧路由块左边为方形模型图标，右边第一行是缩略账号和状态，第二行是会话短号、模型、强度。点击查看该块说明，悬停查看完整账号、ID 和错误。
- 失败使用真实 HTTP 状态及错误；499 为调用方取消，不算账号冷却。后续成功或 `tries>1` 时显示“已恢复”或“重试成功”。`usage.jsonl` 不含路由页的逐次失败账号、实时冷却和精确恢复时间，不能复刻“31 秒后恢复”倒计时。
- **Token**：按系统本地日期汇总 `in + out + cache_read`。日志中的 `t` 是请求开始时间；当天归属按该时间转本地日期，最新完成和恢复顺序按文件追加顺序，避免并发请求倒序。
- **费用**：只合计日志实际提供的 `credits`，标明 **Credits** 与覆盖条数；没有数据时显示“未记录”。Credits 不是美元，当前没有价格表和货币金额，不能推算真实账单。
- **Tks**：当日加权输出速度 `Σout / Σ(ms − ttft_ms) × 1000`，只使用成功且生成耗时有效的记录，不把输入、缓存 Token 或首字等待时间当生成速度。
- 模型图标来自 [LobeHub Icons](https://github.com/lobehub/lobe-icons) 的 `@lobehub/icons-static-svg` 1.95.1，已本地打包并保留 [MIT 许可](ui/icons/LICENSE)，应用运行时不联网加载图标。

## 验证状态

- 2026-10-19：`cargo test --lib` 5 项通过，调试构建通过。隔离进程在追加临时日志后仍存活；该检查不能证明前端已成功渲染或延迟达标。
- 2026-10-20：安装 Tauri CLI 2.12.3，执行 `cargo tauri dev`。修正前端字符串意外断行和 `router-host` 元素缺少 ID 后，实际窗口已显示真实账号、服务模型、统计及两行时间说明；已检查截图与辅助功能控件树。
- 置顶验证中曾关闭置顶，检查结束后配置与原生窗口标志均恢复为开启。按钮自动往返测试未完整通过，不计为全部交互验收。
- 2026-10-20 会话与统计增量：Rust 单元测试、`node --check`、1120px/720px 网页排版检查通过，实际 Tauri 截图已核对。隔离临时日志在真实 WebView2 完成会话切换、失败→恢复、半行处理、Credits 覆盖和当日统计验证，完整写入至界面更新实测约 141ms（单机样本，不代表所有环境）。
- - 2026-10-20 feat-4：MSVC 环境下 `cargo test --lib --locked` 12 项通过，覆盖统计公式、空记录、异常日志、配置边界和窗口约束；`node --check ui/hud.js` 通过。
- 待人工确认：真实请求刷新、窗口/托盘交互、长时间运行、跨日实机、多屏/DPI 热插拔和发布构建。

实际会话路由窗口：

![实际会话路由窗口](docs/showcase/session-routing-window.png)

失败、重试成功和费用覆盖的脱敏演示（用于检查状态，不是生产数据）：

![路由状态演示](docs/showcase/route-sessions-preview.png)

可重复的隔离桌面验收脚本（需要 Python 的 `websocket-client`，先调试构建；脚本只写临时目录，关闭自己启动的测试窗口）：

```bash
python scripts/verify_routing_window.py
```

早期开发窗口截图（2026-10-20；截图中的账号和记录为当时状态）：

![MagpieHUD 实际开发窗口](docs/showcase/Feat_261019_1/img/dev-window.png)

## 变更记录

2026-10-19 | 0.1.0 | 归档路由悬浮窗 SPEC，建立 M1/M2/M3 三份需求档案。

2026-10-19 | 0.1.0 | 按用户授权自行设计 WebView UI，新增独立 HTML 原型及本地预览配置，归档三份 FEAT §2。

2026-10-19 | 0.1.0 | 用 Tauri 2 替换 RSQt 脚手架，接入日志监视、两行说明、三节点、托盘和配置；完整行为待系统验收。

2026-10-20 | 0.1.0 | 修复前端字符串断行及地址元素缺少 ID 引发的启动渲染失败，确认 CLI 2.12.3 开发启动与真实数据画面，归档运行截图。

2026-10-20 | 0.1.0 | 增加当日 Token/Credits/Tks、按会话查看与模型图标两行路由块；区分失败、取消和重试成功，改为增量读取，实测隔离 WebView 刷新约 139ms。

## SOP的开发信息

### Feat

| 编号 | 简述 | 阶段 | 描述 |
| --- | --- | --- | --- |
| [Feat_261019_1](docs/feat/Feat_261019_1_悬浮窗基础闭环.md) | 悬浮窗基础闭环 | 开发 | 程序自测通过；窗口、真实刷新和异常界面待人工确认。 |
| [Feat_261019_2](docs/feat/Feat_261019_2_分流图与托盘.md) | 分流图与托盘 | 开发 | 程序自测通过；图文、动画和托盘待人工确认。 |
| [Feat_261019_3](docs/feat/Feat_261019_3_多屏与配置.md) | 多屏与配置 | 开发 | 配置与边界计算通过；位置恢复和多屏待人工确认。 |
