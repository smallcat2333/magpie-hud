//! 构建 CXX-Qt QObject 和 C++ 桥接代码，产物由 CMake 集成。

/// 声明所有使用 cxx_qt::bridge 的源文件；新增桥接文件时在这里登记。
fn main() {
    cxx_qt_build::CxxQtBuilder::new()
        .file("src/backend.rs")
        .build();
}
