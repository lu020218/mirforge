# MirForge 中心站 (hub) 一键启动 (PowerShell)
# 用法:
#   tools\dev-hub.ps1                       # 起 hub 于 127.0.0.1:4001, 库 target/hub.db
#   tools\dev-hub.ps1 -Import target\dev.db # 先把旧单机库的配置整体导入再启动
#
# hub = 全区唯一的配置权威 + 统一管理后台:
# - 管理台: http://127.0.0.1:4001/  (配置/公告/更新/区服注册表/总览)
# - 登录器 site 指向它; 区服设 MIRFORGE_HUB 指向它 (见 dev-server.ps1 -Hub)
# - 首次建库自动从 server/data/*.json 种子导入; 已有单机库配置用 -Import 搬入
param(
    [string]$Addr = "127.0.0.1:4001",
    [string]$Db = "target/hub.db",
    [string]$Import = ""
)

Set-Location (Join-Path $PSScriptRoot "..")

$env:MIRFORGE_PACKS = Join-Path (Get-Location) "packs"
$env:MIRFORGE_HUB_ADDR = $Addr
$env:MIRFORGE_HUB_DB = $Db

if ($Import -ne "") {
    Write-Host "导入旧库配置: $Import → $Db"
    cargo run -p mirforge-hub -- import --from $Import
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
Write-Host "hub 管理台: http://$Addr/   配置库: $Db"
cargo run -p mirforge-hub
