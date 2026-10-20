# magpie-hud

使用 **Tauri 2 + 系统 WebView** 构建 Windows 独立悬浮窗，只读 Magpie 的 `usage.jsonl`，显示简化分流图与固定两行带本地时间的路由说明。不修改 Magpie、不读取密钥、不集成进 rs_agent_router；MVP 不依赖 HTTP API。

**当前状态：已能启动并显示真实路由记录，M1–M3 尚待系统验收。** 默认窗口 840×280，石墨深色底、洋红三节点流向和底部两行说明。完整需求见 [SPEC](docs/spec/magpie-route-float-SPEC.md)，UI 设计基准见 [M1 §2](docs/feat/Feat_261019_1_悬浮窗基础闭环.md)。[独立 HTML 原型](docs/ui/magpie-hud.html) 使用演示数据，与实际程序入口 `ui/index.html` 分开。

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

日志在请求结束后写入，≤500ms 的刷新目标从完整记录落盘开始计算。完整排除原因待二期对接路由 API；当前第二行可显示明确占位，不表示已获知具体排除原因。

## 验证状态

- 2026-10-19：`cargo test --lib` 5 项通过，调试构建通过。隔离进程在追加临时日志后仍存活；该检查不能证明前端已成功渲染或延迟达标。
- 2026-10-20：安装 Tauri CLI 2.12.3，执行 `cargo tauri dev`。修正前端字符串意外断行和 `router-host` 元素缺少 ID 后，实际窗口已显示真实账号、服务模型、统计及两行时间说明；已检查截图与辅助功能控件树。
- 置顶验证中曾关闭置顶，检查结束后配置与原生窗口标志均恢复为开启。按钮自动往返测试未完整通过，不计为全部交互验收。
- 待验证：连续刷新及 ≤500ms 延迟、文件替换/截断/缺失恢复、坏行处理、托盘隐藏恢复、拖动与重启位置恢复、三屏/DPI/任务栏边界，以及发布构建。

实际开发窗口截图（2026-10-20；截图中的账号和记录为当时状态）：

![MagpieHUD 实际开发窗口](docs/showcase/Feat_261019_1/img/dev-window.png)

## 变更记录

2026-10-19 | 0.1.0 | 归档路由悬浮窗 SPEC，建立 M1/M2/M3 三份需求档案。

2026-10-19 | 0.1.0 | 按用户授权自行设计 WebView UI，新增独立 HTML 原型及本地预览配置，归档三份 FEAT §2。

2026-10-19 | 0.1.0 | 用 Tauri 2 替换 RSQt 脚手架，接入日志监视、两行说明、三节点、托盘和配置；完整行为待系统验收。

2026-10-20 | 0.1.0 | 修复前端字符串断行及地址元素缺少 ID 引发的启动渲染失败，确认 CLI 2.12.3 开发启动与真实数据画面，归档运行截图。

## SOP的开发信息

### Feat

| 编号 | 简述 | 阶段 | 描述 |
| --- | --- | --- | --- |
| [Feat_261019_1](docs/feat/Feat_261019_1_悬浮窗基础闭环.md) | 悬浮窗基础闭环 | 开发 | 启动渲染修正已验证，真实日志可见；刷新和异常场景待验收。 |
| [Feat_261019_2](docs/feat/Feat_261019_2_分流图与托盘.md) | 分流图与托盘 | 开发 | 三节点已实际渲染，动画和托盘完整交互待验收。 |
| [Feat_261019_3](docs/feat/Feat_261019_3_多屏与配置.md) | 多屏与配置 | 开发 | 置顶恢复已确认；位置持久化、多屏及工作区约束待验收。 |
