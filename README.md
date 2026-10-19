# magpie-hud

目标：使用 **Tauri 2 + 系统 WebView** 构建 Windows 独立悬浮窗，只读 Magpie 的 `usage.jsonl`，显示简化分流图与固定两行带本地时间的路由说明。不修改 Magpie，不读取密钥，不集成进 rs_agent_router；MVP 不依赖 HTTP API。

**当前状态：M1–M3 已实现 Tauri 2 悬浮窗。** 程序只读 `usage.jsonl`，显示三节点流向、固定两行说明，并支持托盘、置顶、位置恢复和工作区约束。

UI 基准见 [独立 HTML 原型](docs/ui/magpie-hud.html) 与 [M1 §2 设计总纲](docs/feat/Feat_261019_1_悬浮窗基础闭环.md)。默认 840×280，石墨深色底、洋红三节点流向、底部固定两行时间说明；外围工具可演示正常、等待、缺文件、坏行、缺字段和长文本。示例使用虚构账号，不读取本机日志。原型静态/脚本验证通过；本轮浏览器预览工具仍绑定旧项目目录，未完成视觉截图，原生置顶/拖动/托盘和多屏尚待 Tauri 验证。

完整需求见 [Magpie 路由监视悬浮窗 SPEC](docs/spec/magpie-route-float-SPEC.md)，由用户于 2026-10-19 提供，原文归档。产品和目录名采用用户指定的 `magpie-hud`，覆盖 SPEC 中可改的建议名称 `magpie-route-float`。

优先交付 M1+M2；M3 的多屏工作区与配置要求仍属于完整产品验收。数据在请求完成后写入，≤500ms 的刷新验收从完整记录落盘开始计算。完整排除原因待二期对接路由 API；当前需求允许第二行使用明确占位，不能编造原因。

## 运行

要求 Rust stable、Windows 10/11 WebView2 Runtime 和项目内 Tauri CLI 2。全局 `cargo tauri` 若仍是 1.x，不能用来构建本项目。

```powershell
cargo install tauri-cli --version "^2.0.0" --locked
cargo tauri dev
cargo tauri build
```

默认读取 `%USERPROFILE%\.config\magpie\usage.jsonl`，配置位于 `%APPDATA%\magpie-hud\config.json`。配置可改 `usageJsonlPath`、`magpieHost`、窗口位置和 `alwaysOnTop`；地址只用于显示，不发起 HTTP 请求。第二行在日志没有完整排除原因时显示明确占位。

本程序是 Magpie 的只读外挂监视器，不修改 Magpie，不读取密钥，也不集成进 rs_agent_router。

## 验证状态

`cargo test --lib` 5 项通过，`cargo build` 通过。隔离环境完成启动、追加临时日志、进程保持响应和退出冒烟。真实 Magpie 的 500ms 刷新、系统托盘视觉、三屏/DPI 与任务栏边界尚未在目标桌面完成人工验收。

## 变更记录

2026-10-19 | 0.1.0 | 用 Tauri 2 实现路由悬浮窗，替换 RSQt 脚手架；完成日志尾部监视、两行说明、三节点、托盘、配置与工作区约束。

## SOP的开发信息

### Feat

| 编号 | 简述 | 阶段 | 描述 |
| --- | --- | --- | --- |
| [Feat_261019_1](docs/feat/Feat_261019_1_悬浮窗基础闭环.md) | 悬浮窗基础闭环 | 开发 | T1–T3 完成：Tauri 窗口、尾部解析与两行实时说明。 |
| [Feat_261019_2](docs/feat/Feat_261019_2_分流图与托盘.md) | 分流图与托盘 | 开发 | T4 完成：三节点 SVG、320ms 高亮和系统托盘。 |
| [Feat_261019_3](docs/feat/Feat_261019_3_多屏与配置.md) | 多屏与配置 | 开发 | T5 完成：位置恢复、工作区约束和本地配置；实机多屏待验收。 |

## 变更记录

2026-10-19 | 0.1.0（脚手架版本） | 归档路由悬浮窗 SPEC，建立 M1/M2/M3 三份需求档案，明确后续切换 Tauri 2；本轮未改应用代码。

2026-10-19 | 0.1.0（脚手架版本） | 按用户授权自行设计 WebView UI，新增独立 HTML 原型及本地预览配置，归档三份 FEAT §2；静态/脚本验证通过，浏览器视觉及原生窗口行为待验收。

## SOP的开发信息

### Feat

| 编号 | 简述 | 阶段 | 描述 |
| --- | --- | --- | --- |
| [Feat_261019_1](docs/feat/Feat_261019_1_悬浮窗基础闭环.md) | 悬浮窗基础闭环 | UI | 整窗总纲与独立 HTML 原型已归档，840×280 三段布局；视觉及 Tauri 接入待验收。 |
| [Feat_261019_2](docs/feat/Feat_261019_2_分流图与托盘.md) | 分流图与托盘 | UI | 三节点 SVG、320ms 命中高亮与托盘交互已归档；系统托盘待接入。 |
| [Feat_261019_3](docs/feat/Feat_261019_3_多屏与配置.md) | 多屏与配置 | UI | 窗口恢复、工作区与配置反馈行为已归档，不新增设置页；实机多屏待验收。 |
