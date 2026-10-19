#include "native_webview.h"

#include <Windows.h>
#include <wrl.h>
#include <WebView2.h>
#include <QCoreApplication>
#include <QDir>
#include <QFileInfo>
#include <QHideEvent>
#include <QLabel>
#include <QPointer>
#include <QResizeEvent>
#include <QShowEvent>
#include <QStandardPaths>
#include <QTimer>
#include <QUrl>

using Microsoft::WRL::Callback;
using Microsoft::WRL::ComPtr;

/// 持有 COM 浏览器对象和初始化提示，均只在 GUI 线程使用。
class NativeWebView::Impl {
public:
    ComPtr<ICoreWebView2Environment> environment;
    ComPtr<ICoreWebView2Controller> controller;
    ComPtr<ICoreWebView2> web;
    QLabel *notice = nullptr;
};

NativeWebView::NativeWebView(QWidget *parent)
    : QWidget(parent), impl_(std::make_unique<Impl>()) {
    setAttribute(Qt::WA_NativeWindow);
    impl_->notice = new QLabel(tr("正在准备 WebView2 页面…"), this);
    impl_->notice->setAlignment(Qt::AlignCenter);
    impl_->notice->setWordWrap(true);
}

NativeWebView::~NativeWebView() {
    if (impl_->controller) impl_->controller->Close();
}

void NativeWebView::loadPage(const QString &fileName) {
    if (fileName.isEmpty() || QFileInfo(fileName).fileName() != fileName ||
        !fileName.endsWith(QStringLiteral(".html"), Qt::CaseInsensitive) ||
        fileName == QStringLiteral(".") || fileName == QStringLiteral("..")) {
        fail(tr("WebView2 页面文件名无效"));
        return;
    }
    page_ = fileName;
    if (impl_->web) navigate();
    else if (isVisible()) initialise();
}

void NativeWebView::fail(const QString &message) {
    initialising_ = false;
    impl_->notice->setText(message);
    impl_->notice->show();
    emit loadFinished(false);
}

void NativeWebView::navigate() {
    if (!impl_->web || page_.isEmpty()) return;
    const auto url = (QStringLiteral("https://appassets.example/") +
                      QString::fromLatin1(QUrl::toPercentEncoding(page_))).toStdWString();
    if (FAILED(impl_->web->Navigate(url.c_str()))) fail(tr("WebView2 页面导航失败"));
}

void NativeWebView::initialise() {
    if (initialising_ || impl_->controller || page_.isEmpty()) return;
    const auto assets = QCoreApplication::applicationDirPath() + QStringLiteral("/resources/html");
    if (!QDir(assets).exists() || !QFileInfo::exists(assets + QLatin1Char('/') + page_)) {
        fail(tr("未找到 WebView2 页面资源"));
        return;
    }
    const auto profile = QStandardPaths::writableLocation(QStandardPaths::AppLocalDataLocation) +
                         QStringLiteral("/WebView2");
    if (!QDir().mkpath(profile)) {
        fail(tr("无法创建 WebView2 用户数据目录"));
        return;
    }
    initialising_ = true;
    QPointer<NativeWebView> guard(this);
    const auto dataFolder = profile.toStdWString();
    const HRESULT started = CreateCoreWebView2EnvironmentWithOptions(
        nullptr, dataFolder.c_str(), nullptr,
        Callback<ICoreWebView2CreateCoreWebView2EnvironmentCompletedHandler>(
            [guard, assets](HRESULT hr, ICoreWebView2Environment *environment) -> HRESULT {
                if (!guard) return S_OK;
                if (FAILED(hr) || !environment) {
                    guard->fail(QObject::tr("缺少 Microsoft Edge WebView2 Runtime；请安装后重新打开应用"));
                    return S_OK;
                }
                guard->impl_->environment = environment;
                const HRESULT creating = environment->CreateCoreWebView2Controller(
                    reinterpret_cast<HWND>(guard->winId()),
                    Callback<ICoreWebView2CreateCoreWebView2ControllerCompletedHandler>(
                        [guard, assets](HRESULT controllerHr, ICoreWebView2Controller *controller) -> HRESULT {
                            if (!guard) {
                                if (controller) controller->Close();
                                return S_OK;
                            }
                            if (FAILED(controllerHr) || !controller) {
                                guard->fail(QObject::tr("WebView2 窗口创建失败"));
                                return S_OK;
                            }
                            guard->impl_->controller = controller;
                            if (FAILED(controller->get_CoreWebView2(&guard->impl_->web)) || !guard->impl_->web) {
                                guard->fail(QObject::tr("WebView2 接口不可用"));
                                return S_OK;
                            }
                            ComPtr<ICoreWebView2_3> web3;
                            if (FAILED(guard->impl_->web.As(&web3)) ||
                                FAILED(web3->SetVirtualHostNameToFolderMapping(
                                    L"appassets.example", assets.toStdWString().c_str(),
                                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS))) {
                                guard->fail(QObject::tr("WebView2 本地资源映射失败"));
                                return S_OK;
                            }
                            ComPtr<ICoreWebView2Settings> settings;
                            if (SUCCEEDED(guard->impl_->web->get_Settings(&settings)) && settings) {
                                settings->put_AreDefaultContextMenusEnabled(FALSE);
                                settings->put_AreDevToolsEnabled(FALSE);
                                settings->put_IsStatusBarEnabled(FALSE);
                                settings->put_AreHostObjectsAllowed(FALSE);
                            }
                            EventRegistrationToken token{};
                            guard->impl_->web->add_NavigationStarting(
                                Callback<ICoreWebView2NavigationStartingEventHandler>(
                                    [](ICoreWebView2 *, ICoreWebView2NavigationStartingEventArgs *args) -> HRESULT {
                                        LPWSTR raw = nullptr;
                                        if (FAILED(args->get_Uri(&raw))) return S_OK;
                                        const QUrl url(QString::fromWCharArray(raw ? raw : L""));
                                        CoTaskMemFree(raw);
                                        if (url.scheme() != QStringLiteral("https") ||
                                            url.host() != QStringLiteral("appassets.example"))
                                            args->put_Cancel(TRUE);
                                        return S_OK;
                                    }).Get(), &token);
                            guard->impl_->web->add_NewWindowRequested(
                                Callback<ICoreWebView2NewWindowRequestedEventHandler>(
                                    [](ICoreWebView2 *, ICoreWebView2NewWindowRequestedEventArgs *args) -> HRESULT {
                                        args->put_Handled(TRUE);
                                        return S_OK;
                                    }).Get(), &token);
                            guard->impl_->web->add_PermissionRequested(
                                Callback<ICoreWebView2PermissionRequestedEventHandler>(
                                    [](ICoreWebView2 *, ICoreWebView2PermissionRequestedEventArgs *args) -> HRESULT {
                                        args->put_State(COREWEBVIEW2_PERMISSION_STATE_DENY);
                                        return S_OK;
                                    }).Get(), &token);
                            guard->impl_->web->add_NavigationCompleted(
                                Callback<ICoreWebView2NavigationCompletedEventHandler>(
                                    [guard](ICoreWebView2 *, ICoreWebView2NavigationCompletedEventArgs *args) -> HRESULT {
                                        if (!guard) return S_OK;
                                        BOOL ok = FALSE;
                                        args->get_IsSuccess(&ok);
                                        QTimer::singleShot(0, guard, [guard, ok] {
                                            if (!guard) return;
                                            if (ok) {
                                                guard->impl_->notice->hide();
                                                emit guard->loadFinished(true);
                                            } else guard->fail(QObject::tr("WebView2 页面加载失败"));
                                        });
                                        return S_OK;
                                    }).Get(), &token);
                            guard->initialising_ = false;
                            guard->updateBounds();
                            controller->put_IsVisible(guard->isVisible());
                            guard->navigate();
                            return S_OK;
                        }).Get());
                if (FAILED(creating)) guard->fail(QObject::tr("WebView2 窗口启动失败"));
                return S_OK;
            }).Get());
    if (FAILED(started)) fail(tr("WebView2 运行时启动失败"));
}

void NativeWebView::updateBounds() {
    impl_->notice->setGeometry(rect());
    if (!impl_->controller) return;
    RECT bounds{};
    GetClientRect(reinterpret_cast<HWND>(winId()), &bounds);
    impl_->controller->put_Bounds(bounds);
}

void NativeWebView::resizeEvent(QResizeEvent *event) {
    QWidget::resizeEvent(event);
    updateBounds();
}

void NativeWebView::showEvent(QShowEvent *event) {
    QWidget::showEvent(event);
    initialise();
    if (impl_->controller) impl_->controller->put_IsVisible(TRUE);
}

void NativeWebView::hideEvent(QHideEvent *event) {
    if (impl_->controller) impl_->controller->put_IsVisible(FALSE);
    QWidget::hideEvent(event);
}
