# MirForge 开发客户端一键启动 (PowerShell)
# 用法:
#   tools\dev-client.ps1              # 联机模式, 连 ws://127.0.0.1:4000
#   tools\dev-client.ps1 -Offline    # 离线单机漫游 (不需要服务器)
#
# 引擎唯一资源根 = 仓库根 packs/ (图库 .mfl + 地图 .map, 见 packs/README.md)。
# resources/ 只是素材原始文件的开发态堆场, 引擎不读。
param(
    [string]$Server = "ws://127.0.0.1:4000",
    [switch]$Offline
)

Set-Location (Join-Path $PSScriptRoot "..")

$Packs = Join-Path (Get-Location) "packs"
if (-not (Get-ChildItem $Packs -Recurse -Filter *.mfl -ErrorAction SilentlyContinue | Select-Object -First 1)) {
    Write-Warning "packs/ 下没有 .mfl 图库 — 画面会缺地图/角色/怪物。打包方法见 packs/README.md (mir-pack)。"
}
if (-not (Get-ChildItem (Join-Path $Packs "map") -Filter *.map -ErrorAction SilentlyContinue | Select-Object -First 1)) {
    Write-Error "packs/map 下没有地图 (.map)。用 `mir-pack import-maps <素材目录> packs` 收入地图后再启动。"
    exit 2
}
$env:MIRFORGE_PACKS = $Packs
if ($Offline) {
    Remove-Item Env:MIRFORGE_SERVER -ErrorAction SilentlyContinue
    Write-Host "资源包: $Packs   模式: 离线单机"
} else {
    $env:MIRFORGE_SERVER = $Server
    Write-Host "资源包: $Packs   服务器: $Server"
}
cargo run -p mirforge-client
