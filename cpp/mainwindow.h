#pragma once
#include <QMainWindow>
#include <memory>

namespace Ui { class MainWindow; }
namespace rsqt { class Backend; }
class NativeWebView;

/// 窗口只管理 .ui 控件和信号连接，业务由 Rust Backend 处理。
class MainWindow final : public QMainWindow {
    Q_OBJECT
public:
    /// 在 GUI 线程装配窗口和 Rust QObject；QObject 父子关系管理 Backend 生命周期。
    explicit MainWindow(QWidget *parent = nullptr);
    /// 在 .ui 完整类型可见的源文件中释放界面包装对象。
    ~MainWindow() override;
    /// 使用真实按钮与 Rust 回传做冒烟验证，结果通过应用退出码返回。
    void startSmokeTest();
    /// 等待本地 HTML 在 WebView2 中成功加载，供有桌面的开发机验收。
    void startWebViewSmokeTest();
private:
    std::unique_ptr<Ui::MainWindow> ui_;
    rsqt::Backend *backend_;
    NativeWebView *webview_;
};
