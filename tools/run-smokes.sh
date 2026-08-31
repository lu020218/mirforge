#!/usr/bin/env bash
# 一键跑全部协议级冒烟 (每套独立重启服务器, 世界状态互不污染)。
# 用法: tools/run-smokes.sh  (需要仓库根 packs/ 已就绪, 可用 MIRFORGE_PACKS 覆盖)
set -u
cd "$(dirname "$0")/.."

: "${MIRFORGE_PACKS:=packs}"
export MIRFORGE_PACKS
ADDR="${MIRFORGE_ADDR:-127.0.0.1:4100}"
DB=target/smoke-run.db

cargo build -p mirforge-server --examples || exit 1

fail=0
for t in smoke combat skills items quests; do
  taskkill //IM mirforge-server.exe //F >/dev/null 2>&1
  rm -f "$DB"
  MIRFORGE_DB="$DB" MIRFORGE_ADDR="$ADDR" ./target/debug/mirforge-server.exe >"target/$t.server.log" 2>&1 &
  sleep 3
  if MIRFORGE_ADDR="$ADDR" "./target/debug/examples/$t.exe" >"target/$t.out" 2>&1; then
    echo "$t: PASS"
  else
    echo "$t: FAIL (见 target/$t.out)"
    fail=1
  fi
done
taskkill //IM mirforge-server.exe //F >/dev/null 2>&1
exit $fail
