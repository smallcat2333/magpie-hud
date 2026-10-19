#pragma once

#include <QWidget>
#include <QString>
#include <memory>

/// 在 QWidget 中承载系统 WebView2，并只加载应用随附的 HTML 页面。
class NativeWebView final : public QWidget {
    Q_OBJECT
public:
    /// 创建原生窗口；浏览器在控件显示且指定页面后异步初始化。
    explicit NativeWebView(QWidget *parent = nullptr);
    /// 关闭浏览器控制器，避免异步回调访问已销毁窗口。
    ~NativeWebView() override;
    /// 加载 resources/html 中的单个 HTML 文件名；其他路径会显示错误。
    void loadPage(const QString &fileName);
signals:
    /// 页面完成加载或失败后通知界面；失败原因显示在控件内。
    void loadFinished(bool ok);
protected:
    /// 保持浏览器子窗口与 QWidget 可用区域同步。
    void resizeEvent(QResizeEvent *event) override;
    /// 首次显示时启动 WebView2，并恢复浏览器可见性。
    void showEvent(QShowEvent *event) override;
    /// 隐藏时停止显示浏览器子窗口。
    void hideEvent(QHideEvent *event) override;
private:
    /// 创建系统运行时、控制器并绑定本地 HTML 资源目录。
    void initialise();
    /// 将页面文件名转为映射域名 URL 并导航。
    void navigate();
    /// 显示运行时或资源错误，并发出失败信号。
    void fail(const QString &message);
    /// 同步浏览器窗口和错误提示的尺寸。
    void updateBounds();
    class Impl;
    std::unique_ptr<Impl> impl_;
    QString page_;
    bool initialising_ = false;
};
