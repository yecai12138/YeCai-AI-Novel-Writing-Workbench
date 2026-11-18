# 启动 StoryDesk（Windows / Rust gnu 工具链）
#
# 为什么需要这个脚本：
#   项目路径含中文（D:\桌面\大制作\小说），MinGW 的 ld.exe 打不开非 ASCII 路径下的
#   目标文件，报 "cannot find ...rcgu.o / lib*.rlib: No such file or directory"。
#   解决方式：把 cargo 的构建目录（CARGO_TARGET_DIR）指到纯 ASCII 路径，
#   源码仍留在中文目录里，构建产物落在 C:\storydesk-build。
#
# 用法：
#   pwsh -File scripts\dev.ps1             # 开发模式（热更新窗口）
#   pwsh -File scripts\dev.ps1 -Task build # 打安装包
#   pwsh -File scripts\dev.ps1 -TargetDir D:\build\storydesk   # 自定义构建目录（必须是纯英文路径）

param(
    [ValidateSet("dev", "build")]
    [string]$Task = "dev",
    [string]$TargetDir = "C:\storydesk-build"
)

$ErrorActionPreference = "Stop"

if ($TargetDir -match '[^\x00-\x7F]') {
    throw "构建目录必须是纯 ASCII 路径，当前为：$TargetDir"
}

# 1) MinGW（Rust gnu 目标的链接器）
$mingwCandidates = @(
    "$env:USERPROFILE\.mingw64\mingw64\bin",
    "C:\mingw64\bin",
    "C:\msys64\mingw64\bin"
)
$mingw = $mingwCandidates | Where-Object { Test-Path (Join-Path $_ "gcc.exe") } | Select-Object -First 1
if (-not $mingw) {
    throw "找不到 MinGW-w64 的 gcc.exe；若用 MSVC 工具链，直接在 VS 开发者命令行里跑 npm run tauri $Task。"
}

$env:PATH = "$mingw;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:CARGO_TARGET_DIR = $TargetDir
New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null

Write-Host "MinGW      : $mingw"
Write-Host "rustc      : $(& "$env:USERPROFILE\.cargo\bin\rustc.exe" --version 2>&1)"
Write-Host "target dir : $TargetDir"
Write-Host "项目目录   : $PSScriptRoot\.."

Set-Location (Join-Path $PSScriptRoot "..")
npm.cmd run "tauri:$Task"
