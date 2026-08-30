# packs/ — 自有资源包体系

引擎的自有资源根目录:**自购或自制**的素材打包成 `.mfl` 后按**资源类型**归位,
与 Crystal 原版资源(`resources/`,只读兜底)彻底分离。加载规则:**同类同号,
packs 覆盖 Crystal**;当某类资源全部由 packs 提供时,对应的 Crystal 目录即可删除。

## 目录结构(按类型,不按购买批次)

```
packs/
  weapon/050.mfl      武器外观 (对应物品表"外观"字段, 建议自有资源从 050 起编号)
  armor/050.mfl       衣甲外观
  hair/               发型 (预留)
  monster/            怪物 (预留)
  npc/                NPC (预留)
  manifest.md         来源与授权登记 (人工维护)
```

编号即文件名(三位十进制)。要**替换** Crystal 原版某个外观,提供同号 .mfl
即可(如 `armor/000.mfl` 顶掉原版 0 号布衣),配置无需改动。

## 工具

```
cargo run -p mir-pack -- pack <PNG目录> <输出.mfl>    # LibraryEditor 解包目录 → .mfl
cargo run -p mir-pack -- info <xxx.mfl>              # 帧数统计
cargo run -p mir-pack -- preview <xxx.mfl> <out.png> [起始帧] [帧数]   # 验货预览
```

输入目录格式:逐帧 `NNNNN.PNG` + `Placements/NNNNN.txt`(两行 = X/Y 锚点),
即市售素材常见的 Crystal LibraryEditor 解包形态。

## 帧表约定

角色/武器沿用 Crystal FrameSet.Player:站 `0+dir×4`、走 `32+dir×6`、
跑 `80+dir×6`、攻击 `136+dir×6`(dir = 8 方向,0=北顺时针)。

## 版权

`.mfl` 产物与源素材一律**不入 git**(见根 .gitignore);每次添加素材请在
`manifest.md` 登记来源与授权范围。`MIRFORGE_PACKS` 环境变量可改包根路径,
默认为工作目录下 `packs/`。
