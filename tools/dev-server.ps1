# MirForge 开发服务器一键启动 (PowerShell)
# 用法:
#   tools\dev-server.ps1                 # 单机模式: 自带配置库+完整后台 (4001)
#   tools\dev-server.ps1 -Hub http://127.0.0.1:4001 -ServerId s1
#                                        # hub 模式: 配置来自中心站, 后台在 hub;
#                                        # 本地只留内部运行时 API (默认 4002)
#   tools\dev-server.ps1 -Hub ... -ServerId s2 -Addr 127.0.0.1:4010 -Db target/dev2.db -Internal 127.0.0.1:4012
#                                        # 同机第二区服: 端口与库错开
#
# 引擎唯一资源根 = 仓库根 packs/ (图库 .mfl + 地图 .map, 见 packs/README.md)。
# resources/ 只是素材原始文件的开发态堆场, 引擎不读 —
# 新素材先用 mir-pack 打包/收入 packs 再启动。
param(
    [string]$Addr = "127.0.0.1:4000",
    [string]$Db = "target/dev.db",
    [string]$Hub = "",
    [string]$ServerId = "s1",
    [string]$Internal = ""
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
if ($Hub -ne "") {
    $env:MIRFORGE_HUB = $Hub
    $env:MIRFORGE_SERVER_ID = $ServerId
    if ($Internal -ne "") { $env:MIRFORGE_ADMIN = $Internal }
    Write-Host "hub 模式: 配置来自 $Hub (区服 id: $ServerId)"
} else {
    Write-Host "单机模式: 自带配置库, 管理台 http://127.0.0.1:4001/"
}
Write-Host "资源包: $Packs"
Write-Host "监听: $Addr   存档: $Db"
cargo run -p mirforge-server
