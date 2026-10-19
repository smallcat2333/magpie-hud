#include "mainwindow.h"
#include <QApplication>
#include <QDebug>
#include <objbase.h>

#ifdef Q_OS_WIN
#include <QtCore/qt_windows.h>

/// 保留 Qt 日志格式；控制台使用 Unicode，管道/文件使用 UTF-8，避免中文随代码页丢失。
static void writeTerminalLog(QtMsgType type, const QMessageLogContext &context, const QString &message) {
    const auto formatted = qFormatLogMessage(type, context, message);
    if (formatted.isNull()) return;
    const auto text = formatted + QLatin1Char('\n');
    const auto handle = GetStdHandle(STD_ERROR_HANDLE);
    DWORD mode = 0;
    DWORD written = 0;
    if (GetConsoleMode(handle, &mode)) {
        WriteConsoleW(handle, reinterpret_cast<const wchar_t *>(text.utf16()),
                      static_cast<DWORD>(text.size()), &written, nullptr);
    } else {
        const auto bytes = text.toUtf8();
        WriteFile(handle, bytes.constData(), static_cast<DWORD>(bytes.size()), &written, nullptr);
    }
}
#endif

/// 初始化 Qt 事件循环；两个冒烟入口分别验证 Rust 任务和本地 WebView2 页面。
int main(int argc, char *argv[]) {
#ifdef Q_OS_WIN
    if (qEnvironmentVariableIntValue("QT_FORCE_STDERR_LOGGING")) qInstallMessageHandler(writeTerminalLog);
#endif
    QApplication app(argc, argv);
    QCoreApplication::setApplicationName(QStringLiteral("magpie_hud"));
    QCoreApplication::setApplicationVersion(QStringLiteral(APP_VERSION));
    const HRESULT com = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
    if (FAILED(com)) {
        qCritical("WebView2 requires a GUI STA COM apartment");
        return 1;
    }
    int result = 1;
    {
        MainWindow window;
        if (app.arguments().contains(QStringLiteral("--smoke-test"))) {
            window.startSmokeTest();
        } else if (app.arguments().contains(QStringLiteral("--webview-smoke"))) {
            window.startWebViewSmokeTest();
        }
        window.show();
        result = app.exec();
    }
    CoUninitialize();
    return result;
}
