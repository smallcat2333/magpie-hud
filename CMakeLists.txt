cmake_minimum_required(VERSION 3.24)

# Rust MSVC 使用 /MD；所有 C++ 目标保持同一运行库，Debug 也链接 Qt Release DLL。
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreadedDLL")
file(STRINGS "${CMAKE_CURRENT_SOURCE_DIR}/rust/Cargo.toml" APP_VERSION_LINE REGEX "^version = ")
string(REGEX REPLACE "^version = \"([^\"]+)\".*" "\\1" APP_VERSION "${APP_VERSION_LINE}")
project(magpie_hud VERSION ${APP_VERSION} LANGUAGES CXX)
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(CMAKE_AUTOUIC ON)
set(CMAKE_AUTORCC ON)
set(CMAKE_AUTOUIC_SEARCH_PATHS "${CMAKE_CURRENT_SOURCE_DIR}/ui")
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")

find_package(Qt6 6.8 REQUIRED COMPONENTS Core Gui Widgets)
set(WEBVIEW2_SDK "" CACHE PATH "Microsoft Edge WebView2 SDK root")
if(NOT EXISTS "${WEBVIEW2_SDK}/build/native/include/WebView2.h" OR
   NOT EXISTS "${WEBVIEW2_SDK}/build/native/x64/WebView2LoaderStatic.lib")
    message(FATAL_ERROR "WebView2 SDK is missing; set WEBVIEW2_SDK to the extracted SDK root")
endif()
if(MSVC)
    set_property(TARGET Qt6::Core Qt6::Gui Qt6::Widgets PROPERTY MAP_IMPORTED_CONFIG_DEBUG RELEASE)
    add_compile_options(/utf-8)
    add_compile_definitions(_ITERATOR_DEBUG_LEVEL=0)
endif()

# CXX-Qt CMake 集成与 Cargo 依赖固定同版，避免跟随上游开发分支。
include(FetchContent)
FetchContent_Declare(CxxQt
    GIT_REPOSITORY https://github.com/KDAB/cxx-qt-cmake.git
    GIT_TAG 0.10.0
    GIT_SHALLOW TRUE
)
FetchContent_MakeAvailable(CxxQt)
cxx_qt_import_crate(
    MANIFEST_PATH rust/Cargo.toml
    CRATES app_backend
    LOCKED
    QMAKE "$ENV{QMAKE}"
    QT_MODULES Qt6::Core Qt6::Gui Qt6::Widgets
)

# 内部 target 加前缀，允许应用名使用 test / all 等 CMake 保留目标名称。
qt_add_executable(rs_magpie_hud
    cpp/main.cpp cpp/mainwindow.cpp cpp/mainwindow.h
    cpp/native_webview.cpp cpp/native_webview.h ui/mainwindow.ui ui/qrc.qrc
)
target_include_directories(rs_magpie_hud PRIVATE "${WEBVIEW2_SDK}/build/native/include")
target_link_libraries(rs_magpie_hud PRIVATE app_backend Qt6::Widgets
    "${WEBVIEW2_SDK}/build/native/x64/WebView2LoaderStatic.lib" ole32)
target_compile_definitions(rs_magpie_hud PRIVATE APP_VERSION="${PROJECT_VERSION}")
add_custom_target(rs_magpie_hud_assets ALL
    COMMAND "${CMAKE_COMMAND}" -E make_directory "$<TARGET_FILE_DIR:rs_magpie_hud>/resources/html"
    COMMAND "${CMAKE_COMMAND}" -E copy_directory
        "${CMAKE_CURRENT_SOURCE_DIR}/resources/html"
        "$<TARGET_FILE_DIR:rs_magpie_hud>/resources/html"
    VERBATIM)
add_dependencies(rs_magpie_hud rs_magpie_hud_assets)
set_target_properties(rs_magpie_hud PROPERTIES
    OUTPUT_NAME "magpie_hud"
    WIN32_EXECUTABLE TRUE
)

enable_testing()
add_test(NAME gui_rust_bridge COMMAND rs_magpie_hud --smoke-test)
set_tests_properties(gui_rust_bridge PROPERTIES TIMEOUT 15 ENVIRONMENT "QT_QPA_PLATFORM=offscreen")
