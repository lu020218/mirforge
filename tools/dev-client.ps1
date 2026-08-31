# MirForge 开发客户端一键启动 (PowerShell)
# 用法:
#   tools\dev-client.ps1                  # 联机模式, 连 ws://127.0.0.1:4000
#   tools\dev-client.ps1 -Offline        # 离线单机漫游 (不需要服务器)
#   tools\dev-client.ps1 -Res D:\mir-res # 显式指定资源目录
#
# 资源分两处: MIRFORGE_RES 只提供 .map 地图文件 (含 Map/ 子目录);
# 图库一律走 packs/ (.mfl, 见 packs/README.md), 缺了会当场提醒。
param(
    [string]$Res = $env:MIRFORGE_RES,
    [string]$Server = "ws://127.0.0.1:4000",
    [switch]$Offline
)

Set-Location (Join-Path $PSScriptRoot "..")

# 显式路径无效时回退自动探测 (环境变量残留旧路径的常见坑)
if ($Res -and -not (Test-Path (Join-Path $Res "Map"))) {
    Write-Warning "MIRFORGE_RES=$Res 无效 (缺 Map/ 子目录), 回退自动探测"
    $Res = $null
}
if (-not $Res) {
    # 默认使用仓库内 resources/ (不入库, 见 .gitignore)
    $c = Join-Path (Get-Location) "resources"
    if (Test-Path (Join-Path $c "Map")) { $Res = $c }
}
if (-not $Res) {
    Write-Error "找不到地图资源目录 (需含 Map/ 子目录, 内放 .map 文件)。请将资源放入仓库根 resources/, 或设置 `$env:MIRFORGE_RES / -Res 参数。"
    exit 2
}

# 图库包根: 固定用仓库根 packs/ (绝对路径, 不受启动目录影响)
$Packs = Join-Path (Get-Location) "packs"
if (-not (Test-Path $Packs) -or -not (Get-ChildItem $Packs -Recurse -Filter *.mfl -ErrorAction SilentlyContinue | Select-Object -First 1)) {
    Write-Warning "packs/ 下没有 .mfl 图库 — 画面会缺地图/角色/怪物。打包方法见 packs/README.md (mir-pack)。"
}
$env:MIRFORGE_PACKS = $Packs

$env:MIRFORGE_RES = $Res
if ($Offline) {
    Remove-Item Env:MIRFORGE_SERVER -ErrorAction SilentlyContinue
    Write-Host "地图资源: $Res   图库包: $Packs   模式: 离线单机"
} else {
    $env:MIRFORGE_SERVER = $Server
    Write-Host "地图资源: $Res   图库包: $Packs   服务器: $Server"
}
cargo run -p mirforge-client
