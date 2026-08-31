# MirForge 开发服务器一键启动 (PowerShell)
# 用法:
#   tools\dev-server.ps1
#
# 引擎唯一资源根 = 仓库根 packs/ (图库 .mfl + 地图 .map, 见 packs/README.md)。
# resources/ 只是素材原始文件的开发态堆场, 引擎不读 —
# 新素材先用 mir-pack 打包/收入 packs 再启动。
param(
    [string]$Addr = "127.0.0.1:4000",
    [string]$Db = "target/dev.db"
)

Set-Location (Join-Path $PSScriptRoot "..")

$Packs = Join-Path (Get-Location) "packs"
if (-not (Get-ChildItem $Packs -Recurse -Filter *.mfl -ErrorAction SilentlyContinue | Select-Object -First 1)) {
    Write-Warning "packs/ 下没有 .mfl 图库 — 画面与预览会是空的。打包方法见 packs/README.md (mir-pack)。"
}
if (-not (Get-ChildItem (Join-Path $Packs "map") -Filter *.map -ErrorAction SilentlyContinue | Select-Object -First 1)) {
    Write-Error "packs/map 下没有地图 (.map)。用 `mir-pack import-maps <素材目录> packs` 收入地图后再启动。"
    exit 2
}
$env:MIRFORGE_PACKS = $Packs
$env:MIRFORGE_ADDR = $Addr
$env:MIRFORGE_DB = $Db
Write-Host "资源包: $Packs"
Write-Host "监听: $Addr   存档: $Db"
cargo run -p mirforge-server
