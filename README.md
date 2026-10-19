# magpie_hud

Rust + CXX-Qt 0.10 + Qt6 Widgets + WebView2，应用标识 magpie_hud，初始版本 0.1.0。

~~~text
ui/mainwindow.ui        Qt Designer 界面真源
ui/qrc.qrc              应用资源清单，可在 Designer 中编辑
ui/img/                随应用携带的图标
cpp/                   Qt 窗口、控件与信号连接
cpp/native_webview.*   原生 WebView2 页面控件
resources/html/        随应用发布的本地 HTML 页面
rust/src/backend.rs    Rust QObject / Qt 线程回传边界
rust/src/business.rs   纯 Rust 业务
rust/Cargo.toml        应用版本和 Rust 依赖（独立 app_backend crate）
build.ps1              检查、构建、运行、测试、目录打包
dist/                  发布目录
~~~

默认使用 Windows 自带的 Windows PowerShell 5.1，PowerShell 7 也可运行：

~~~powershell
powershell.exe -NoProfile -File .\build.ps1 -Action check
powershell.exe -NoProfile -File .\build.ps1 -Action run
powershell.exe -NoProfile -File .\build.ps1 -Action test
powershell.exe -NoProfile -File .\build.ps1 -Action package
~~~

不传 `-Action` 时默认编译并打开窗口。在传统 PowerShell 中也可直接执行
`.\build.ps1`；在 PowerShell 7 中直接执行则由当前版本运行，不会自动切换。
`build.ps1` 请保持 UTF-8 BOM 编码，以便传统 PowerShell 正确读取中文。

构建读取本机 %LOCALAPPDATA%\Smallcat\rsqt\toolchain.json，也可通过
-Toolchain 指定其他配置。首次构建需要联网下载 Rust 依赖和固定版本的
CXX-Qt CMake 集成；首次产生的 rust/Cargo.lock 应纳入应用版本管理。
编译缓存位于 %LOCALAPPDATA%\Smallcat\rsqt\b 下，以项目绝对路径哈希隔离，
构建时打印实际路径。使用短路径和固定后端 crate 名，规避 MSVC 的路径长度限制。

继续用现有 PySide6 Designer 编辑 .ui。构建由 CMake AUTOUIC 自动生成 C++
头文件，不运行 pyside6-uic，不手改生成产物。Python 自定义控件不能直接放入
此工程，需按实际需求提供 C++ 控件。模板使用 Qt 原生按钮。

初始窗口的 styleSheet 继承 Tools 创建器的 qss_base.ui，后续在 Designer 中
选择 MainWindow 编辑样式。模板自带“执行示例”按钮图标，使用应用内的
ui/qrc.qrc 与 ui/img/play-circle.svg；资源路径为 :/common/img/play-circle.svg。
添加图片时先放入 ui/img/，再在 Designer 资源编辑器中加入 qrc.qrc。
CMake AUTORCC 会自动把资源编进 EXE，修改 .ui、.qrc 或图标后重新 build 即可。

业务入口在 Rust，耗时任务进入后台线程，通过 CXX-Qt 队列回 Qt 线程更新属性。
QObject 和控件只在 GUI 线程访问；任务持有输入数据的副本。示例任务不访问网络
或数据库；实际长任务需要按业务补取消与退出等待，不能跨线程直接操作窗口。

版本真源为 rust/Cargo.toml，CMake 读取后写入 Qt 应用版本。Debug 与 Release
都使用 MSVC /MD 和 Qt Release DLL，并采用 GUI 子系统，均不另开控制台。
通过 build.ps1 运行时，编译信息和应用的 Qt 日志、标准输出显示在原终端；
脚本只让应用附加已有终端。直接双击 EXE 时显示应用窗口，没有终端日志窗口。
package 使用
windeployqt 和 PE 依赖扫描收集动态库，并复制 resources/html；产物为目录，
不能仅复制 exe。

WebView2 默认显示 resources/html/magpie_hud_1.html。页面经 WebView2 虚拟域名
映射加载，允许同目录的相对 CSS、JS 和图片资源；页面导航限制在应用资源域名。
修改页面后重新 build。开发机需要 WebView2 SDK：build.ps1 默认读取
%LOCALAPPDATA%\RSDeployBuild\WebView2，也可传 -WebView2Sdk 指定解压后的 SDK 根目录。
目标机器需安装 Microsoft Edge WebView2 Evergreen Runtime；缺失时控件显示错误，
Qt 原生界面仍可打开。Runtime 不随 package 目录打包；发布安装流程需确保已安装。
在有桌面的开发机上，可对构建产物运行 `magpie_hud.exe --webview-smoke`；
退出码 0 表示本地 HTML 已由 WebView2 加载，1 表示失败，
2 表示十秒内未完成。普通 `-Action test` 使用离屏 Qt 平台，只验证原生 GUI/Rust 链路。
参见 [Microsoft WebView2 分发说明](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution)。

Qt 采用社区 conda-forge 的开源开发包。分发来源不改变许可证要求；正式发布时
须按实际 Qt 模块及第三方库随附许可证、版权与所需源码获取信息。参见
[Qt 许可说明](https://doc.qt.io/qt-6/licensing.html)、
[conda-forge Qt 包说明](https://conda-forge.org/blog/2026/07/01/qt6-status-in-conda-forge/)。
