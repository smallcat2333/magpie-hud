//! CXX-Qt QObject：接收 GUI 请求，将纯业务放到 Rust 线程，再回到 Qt 线程更新状态。
use cxx_qt::Threading;
use cxx_qt_lib::QString;
use std::pin::Pin;

#[cxx_qt::bridge(namespace = "rsqt")]
pub mod ffi {
    #[namespace = ""]
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        /// 仅在 Qt 线程使用的 Qt 字符串边界类型。
        type QString = cxx_qt_lib::QString;
    }
    extern "RustQt" {
        /// 由 C++ 窗口持有的 QObject；状态通知自动生成 Qt signals。
        #[qobject]
        #[qproperty(QString, status)]
        #[qproperty(bool, busy)]
        type Backend = super::BackendRust;

        /// 接收用户文本并启动后台示例；正在执行时忽略重复启动。
        #[qinvokable]
        #[cxx_name = "runExample"]
        fn run_example(self: Pin<&mut Backend>, input: &QString);
    }
    impl cxx_qt::Threading for Backend {}
}

/// QObject 的 Rust 状态，属性读写仅发生在所属 Qt 线程。
#[derive(Default)]
pub struct BackendRust {
    /// 显示结果或失败原因。
    status: QString,
    /// 当前是否有正在执行的任务。
    busy: bool,
}

impl ffi::Backend {
    /// 拷贝输入到后台线程；通过受生命周期保护的队列回传，绝不跨线程访问控件。
    pub fn run_example(mut self: Pin<&mut Self>, input: &QString) {
        if *self.busy() {
            return;
        }
        let text = input.to_string();
        let qt_thread = self.qt_thread();
        self.as_mut().set_status(QString::from("处理中…"));
        self.as_mut().set_busy(true);
        let started = std::thread::Builder::new()
            .name("rsqt-example".to_owned())
            .spawn(move || {
                let result = crate::business::process_message(&text)
                    .unwrap_or_else(|error| format!("执行失败：{error}"));
                // 窗口销毁后队列返回错误，已完成的后台结果可直接释放。
                if let Err(error) = qt_thread.queue(move |mut backend| {
                    backend.as_mut().set_status(QString::from(&result));
                    backend.as_mut().set_busy(false);
                }) {
                    eprintln!("窗口已关闭，结果回传终止：{error:?}");
                }
            });
        if let Err(error) = started {
            self.as_mut()
                .set_status(QString::from(&format!("任务启动失败：{error}")));
            self.as_mut().set_busy(false);
        }
    }
}
