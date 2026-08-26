# MirForge 开发客户端一键启动 (PowerShell)
# 用法:
#   tools\dev-client.ps1                  # 联机模式, 连 ws://127.0.0.1:4000
#   tools\dev-client.ps1 -Offline        # 离线单机漫游 (不需要服务器)
#   tools\dev-client.ps1 -Res D:\mir-res # 显式指定资源目录
param(
    [string]$Res = $env:MIRFORGE_RES,
    [string]$Server = "ws://127.0.0.1:4000",
    [switch]$Offline
)

Set-Location (Join-Path $PSScriptRoot "..")

if (-not $Res) {
    # 默认使用仓库内 resources/ (不入库, 见 .gitignore)
    $c = Join-Path (Get-Location) "resources"
    if (Test-Path (Join-Path $c "Map")) { $Res = $c }
}
if (-not $Res -or -not (Test-Path (Join-Path $Res "Map"))) {
    Write-Error "找不到传奇资源目录 (需含 Map/ 子目录)。请设置 `$env:MIRFORGE_RES 或用 -Res 参数指定。"
    exit 2
}

$env:MIRFORGE_RES = $Res
if ($Offline) {
    Remove-Item Env:MIRFORGE_SERVER -ErrorAction SilentlyContinue
    Write-Host "资源: $Res   模式: 离线单机"
} else {
    $env:MIRFORGE_SERVER = $Server
    Write-Host "资源: $Res   服务器: $Server"
}
cargo run -p mirforge-client
