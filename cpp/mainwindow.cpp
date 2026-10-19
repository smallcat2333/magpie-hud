#include "mainwindow.h"
#include "native_webview.h"
#include "ui_mainwindow.h"
#include <app_backend/src/backend.cxxqt.h>
#include <QCoreApplication>
#include <QPushButton>
#include <QTimer>
#include <QVBoxLayout>

MainWindow::MainWindow(QWidget *parent)
    : QMainWindow(parent), ui_(std::make_unique<Ui::MainWindow>()),
      backend_(new rsqt::Backend(this)) {
    ui_->setupUi(this);
    auto *webLayout = new QVBoxLayout(ui_->widget_webview_host);
    webLayout->setContentsMargins(0, 0, 0, 0);
    webview_ = new NativeWebView(ui_->widget_webview_host);
    webLayout->addWidget(webview_);
    webview_->loadPage(QStringLiteral("magpie_hud_1.html"));
    connect(ui_->pushButton_demo_run, &QPushButton::clicked, this, [this] {
        backend_->runExample(ui_->lineEdit_demo_input->text());
    });
    connect(backend_, &rsqt::Backend::statusChanged, this, [this] {
        ui_->label_demo_result->setText(backend_->getStatus());
    });
    connect(backend_, &rsqt::Backend::busyChanged, this, [this] {
        ui_->pushButton_demo_run->setEnabled(!backend_->getBusy());
        ui_->pushButton_demo_run->setText(backend_->getBusy() ? tr("处理中…") : tr("执行示例"));
    });
}

MainWindow::~MainWindow() = default;

void MainWindow::startSmokeTest() {
    connect(backend_, &rsqt::Backend::busyChanged, this, [this] {
        if (!backend_->getBusy()) {
            const auto expected = QStringLiteral("Rust 已处理：中文验证");
            const bool ok = ui_->label_demo_result->text() == expected;
            QCoreApplication::exit(ok ? 0 : 1);
        }
    });
    QTimer::singleShot(8000, this, [] { QCoreApplication::exit(2); });
    QTimer::singleShot(0, this, [this] {
        ui_->lineEdit_demo_input->setText(QStringLiteral("中文验证"));
        ui_->pushButton_demo_run->click();
    });
}

void MainWindow::startWebViewSmokeTest() {
    connect(webview_, &NativeWebView::loadFinished, this, [](bool ok) {
        QCoreApplication::exit(ok ? 0 : 1);
    });
    QTimer::singleShot(10000, this, [] { QCoreApplication::exit(2); });
}
