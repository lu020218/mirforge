# MirForge 开发登录器一键启动 (PowerShell)
# 用法:
#   tools\dev-launcher.ps1           # 先编译客户端 (登录器要拉起它), 再启动登录器
#   tools\dev-launcher.ps1 -SkipClientBuild   # 客户端已编译过时跳过
#
# 登录器开发态开箱即用:
# - servers.json / launcher.json 生成在 target/debug/ (已 gitignore)
# - 自动探测同目录 mirforge-client.exe 并以仓库根为工作目录拉起 (packs 相对定位)
# - 需要 dev-server.ps1 起着服务器才能登录/看公告; 更新源 = 服务器 updates/ 目录
param(
    [switch]$SkipClientBuild
)

Set-Location (Join-Path $PSScriptRoot "..")

if (-not $SkipClientBuild) {
    Write-Host "编译客户端 (登录器启动游戏用)..."
    cargo build -p mirforge-client
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
cargo run -p mirforge-launcher
