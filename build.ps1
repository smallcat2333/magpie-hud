#requires -Version 5.1
<#
.SYNOPSIS
使用本机 RSQt 工具链检查、构建、运行、测试或打包当前应用。
.DESCRIPTION
Qt DLL 仅加入当前进程环境。编译缓存使用本机短目录，发布目录位于 dist；
任何子命令失败立即终止，package 固定使用 Release，保留已有发布目录。
#>
param(
    [ValidateSet('check', 'build', 'run', 'test', 'package')]
    [string]$Action = 'run',
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',
    [string]$Toolchain = (Join-Path $env:LOCALAPPDATA 'Smallcat\rsqt\toolchain.json'),
    [string]$WebView2Sdk = (Join-Path $env:LOCALAPPDATA 'RSDeployBuild\WebView2')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Invoke-Checked {
    <# 执行原生命令并保留输出；非零退出码转为 PowerShell 异常，阻止误报成功。 #>
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

$config = Get-Content -LiteralPath $Toolchain -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($path in @($config.qt_prefix, $config.cmake, $config.ninja, $config.vs_path)) {
    if (-not (Test-Path -LiteralPath $path)) { throw "Toolchain path does not exist: $path" }
}
foreach ($file in @('build\native\include\WebView2.h', 'build\native\x64\WebView2LoaderStatic.lib')) {
    if (-not (Test-Path -LiteralPath (Join-Path $WebView2Sdk $file))) {
        throw "WebView2 SDK is missing: $(Join-Path $WebView2Sdk $file). Pass -WebView2Sdk."
    }
}
$devShell = Join-Path $config.vs_path 'Common7\Tools\Launch-VsDevShell.ps1'
& $devShell -SkipAutomaticLocation -Arch amd64 -HostArch amd64 | Out-Null
$qtBin = Join-Path $config.qt_prefix 'bin'
$env:PATH = "$qtBin;$(Split-Path $config.cmake);$(Split-Path $config.ninja);$env:PATH"
$env:QMAKE = Join-Path $qtBin 'qmake6.exe'
$qtPlugins = Invoke-Checked $env:QMAKE @('-query', 'QT_INSTALL_PLUGINS')
$env:QT_PLUGIN_PATH = $qtPlugins
$env:QT_QPA_PLATFORM_PLUGIN_PATH = Join-Path $qtPlugins 'platforms'
$env:CXX = (Get-Command cl.exe -ErrorAction Stop).Source
$manifest = Join-Path $PSScriptRoot 'rust\Cargo.toml'

Push-Location $PSScriptRoot
try {
    if ($Action -eq 'check') {
        Invoke-Checked 'rustc' @('--version')
        Invoke-Checked 'cargo' @('--version')
        Invoke-Checked $config.cmake @('--version')
        Invoke-Checked $env:QMAKE @('--version')
        Write-Output "MSVC: $env:VCToolsVersion"
        Write-Output "WebView2 SDK: $WebView2Sdk"
        return
    }
    if ($Action -eq 'package') { $Configuration = 'Release' }
    # CXX-Qt 的生成目录很深；独立项目使用固定后端名与短缓存，规避 MSVC MAX_PATH。
    $pathBytes = [Text.Encoding]::UTF8.GetBytes($PSScriptRoot.ToLowerInvariant())
    # 使用 Windows PowerShell 5.1 支持的 .NET 接口，保持已有缓存键不变。
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $cacheKey = ([BitConverter]::ToString($hasher.ComputeHash($pathBytes))).Replace('-', '').Substring(0, 12).ToLowerInvariant()
    }
    finally {
        $hasher.Dispose()
    }
    $cacheRoot = Join-Path $env:LOCALAPPDATA "Smallcat\rsqt\b\$cacheKey"
    $env:CARGO_TARGET_DIR = Join-Path $cacheRoot 'tests'
    if (-not (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'rust\Cargo.lock'))) {
        Invoke-Checked 'cargo' @('generate-lockfile', '--manifest-path', $manifest)
    }
    $buildDir = Join-Path $cacheRoot $Configuration.Substring(0, 1).ToLowerInvariant()
    Write-Output "Build cache: $buildDir"
    Invoke-Checked $config.cmake @(
        '-S', $PSScriptRoot, '-B', $buildDir, '-G', 'Ninja',
        "-DCMAKE_MAKE_PROGRAM=$($config.ninja)",
        "-DCMAKE_PREFIX_PATH=$($config.qt_prefix)",
        "-DCMAKE_BUILD_TYPE=$Configuration",
        "-DWEBVIEW2_SDK=$WebView2Sdk"
    )
    Invoke-Checked $config.cmake @('--build', $buildDir, '--parallel')
    $executable = Join-Path $buildDir 'bin\magpie_hud.exe'
    switch ($Action) {
        'run' {
            # GUI 应用仅附加现有终端，Qt 日志及标准输出留在当前窗口；退出后还原设置。
            $previousConsole = $env:QT_WIN_DEBUG_CONSOLE
            $previousStderr = $env:QT_FORCE_STDERR_LOGGING
            try {
                $env:QT_WIN_DEBUG_CONSOLE = 'attach'
                $env:QT_FORCE_STDERR_LOGGING = '1'
                $appProcess = Start-Process -FilePath $executable -WorkingDirectory $PSScriptRoot -NoNewWindow -Wait -PassThru
                if ($appProcess.ExitCode -ne 0) { throw "Application exited: $($appProcess.ExitCode)" }
            }
            finally {
                $env:QT_WIN_DEBUG_CONSOLE = $previousConsole
                $env:QT_FORCE_STDERR_LOGGING = $previousStderr
            }
        }
        'test' {
            Invoke-Checked 'cargo' @('test', '--locked', '--manifest-path', $manifest)
            Invoke-Checked (Join-Path (Split-Path $config.cmake) 'ctest.exe') @('--test-dir', $buildDir, '--output-on-failure')
        }
        'package' {
            $distDir = Join-Path $PSScriptRoot ('dist\magpie_hud_' + (Get-Date -Format 'yyyyMMdd_HHmmss'))
            New-Item -ItemType Directory -Path $distDir | Out-Null
            Copy-Item -LiteralPath $executable -Destination $distDir
            $distExe = Join-Path $distDir 'magpie_hud.exe'
            Invoke-Checked (Join-Path $qtBin 'windeployqt6.exe') @(
                '--qtpaths', (Join-Path $qtBin 'qtpaths6.exe'),
                '--release', '--no-translations', '--dir', $distDir, $distExe
            )
            # 社区 Qt 的 ICU、压缩库等额外依赖通过 PE 导入表递归收集。
            Invoke-Checked $config.cmake @(
                "-DAPP_EXE=$distExe", "-DQT_BIN=$qtBin", "-DDESTINATION=$distDir",
                '-P', (Join-Path $PSScriptRoot 'cmake\deploy.cmake')
            )
            Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources') -Destination $distDir -Recurse
            Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README.md') -Destination $distDir
            Write-Output "Package: $distDir"
        }
    }
}
finally {
    Pop-Location
}
