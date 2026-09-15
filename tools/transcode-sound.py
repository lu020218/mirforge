# -*- coding: utf-8 -*-
"""音频转码: resources/Sound (Crystal 版 wav) → packs/sound/*.ogg

用法: python tools/transcode-sound.py   (需要 ffmpeg 在 PATH)

产物布局 (客户端按名字直读, 缺文件静默跳过):
  hum/walk_l|walk_r|run_l|run_r|swing|struck|die_m|die_f.ogg   人物基础
  ui/levelup|gold.ogg                                          界面
  bgm/login.ogg                                                登录主题曲
  mon/{基址:03}-{动作}.ogg   动作: 0出场 1攻击 2受击 3死亡 4特殊
  magic/{技能fx名}_cast|_hit.ogg                               技能施放/命中

技能对位表按经典编号初配 (听感不合直接替换对应 ogg 即可);
新技能加音: 在 MAGIC 表补一行重跑, 或手工放 ogg。
"""
import os
import re
import subprocess

S = "resources/Sound"


def enc(src, dst):
    if not os.path.exists(src):
        print("缺源文件:", src)
        return False
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    r = subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error", "-i", src,
         "-c:a", "libvorbis", "-q:a", "4", dst],
        capture_output=True,
    )
    return r.returncode == 0 and os.path.exists(dst)


# 人物基础 (经典编号: 1-4 走跑左右脚, 52 挥砍, 60 受击, 144/145 男女倒地)
for name, wav in [("walk_l", "1"), ("walk_r", "2"), ("run_l", "3"), ("run_r", "4"),
                  ("swing", "52"), ("struck", "60"), ("die_m", "144"), ("die_f", "145")]:
    enc(f"{S}/{wav}.wav", f"packs/sound/hum/{name}.ogg")

# 界面与 BGM
enc(f"{S}/levelup.wav", "packs/sound/ui/levelup.ogg")
enc(f"{S}/100.wav", "packs/sound/ui/gold.ogg")
enc(f"{S}/Main.wav", "packs/sound/bgm/login.ogg")

# 技能: fx 名 → (施放 wav, 命中 wav)
MAGIC = {
    "huoqiu": ("103", "104"),
    "leidian": ("110", "111"),
    "bingpaoxiao": ("113", "114"),
    "youhuo": ("117", None),
    "zhiyu": ("107", None),
    "shidu": ("112", None),
    "huofu": ("115", "116"),
    "zhaohuan": ("118", None),
    "liehuo": ("125", None),
    "yeman": (None, "91"),
    "shizihou": ("126", None),
}
for fx, (c, h) in MAGIC.items():
    if c:
        enc(f"{S}/{c}.wav", f"packs/sound/magic/{fx}_cast.ogg")
    if h:
        enc(f"{S}/{h}.wav", f"packs/sound/magic/{fx}_hit.ogg")

# 怪物: NNN-K.wav 全量 (K: 0出场 1攻击 2受击 3死亡 4特殊)
pat = re.compile(r"^(\d{3})-(\d)\.wav$", re.I)
n = 0
for f in os.listdir(S):
    m = pat.match(f)
    if m and enc(f"{S}/{f}", f"packs/sound/mon/{m.group(1)}-{m.group(2)}.ogg"):
        n += 1
print("怪物音效", n, "个转码完成")
