# packs/ — 自有资源包体系

引擎的**唯一**图库来源:所有图库资源以 `.mfl` 格式按类型存放于此,
Crystal 资源路线已废除 — 引擎不再读取任何 `.Lib`。原版资源用
`mir-pack convert` 一次性转码进来;自购/自制素材用 `pack`/`pack-list`
打包进来,同号文件即替换。`resources/` 目前仅剩 `.map` 地图文件还在使用
(地图格式迁移是独立课题)。

## 目录结构(按类型,不按购买批次)

```
packs/
  items.mfl           物品图标库 (整库, 帧号 = 物品表"图标帧"字段)
  magicon.mfl         技能图标库 (MagIcon 帧号)
  mmap.mfl            小地图库 (帧号 = 区域配置的小地图帧)
  magic/000.mfl       技能特效 (000=Magic, 001=Magic2)
  weapon/050.mfl      武器外观 (对应物品表"外观"字段, 自有素材建议从 050 起编号)
  armor/050.mfl       衣甲外观
  hair/               发型
  monster/            怪物
  npc/                NPC
  map/<套>/<名>.mfl   地图图库 (保持原相对路径, 如 map/WemadeMir2/Tiles.mfl)
  portrait/050.mfl    人物面板立绘 (帧 0=男 1=女, 编号对应衣甲外观号)
  portrait/naked.mfl  裸模立绘 (未穿衣甲时用, 帧 0=男 1=女)
  manifest.md         来源与授权登记 (人工维护)
```

编号即文件名(三位十进制)。要**替换**某个外观,放同号 .mfl 顶掉即可
(如重打 `armor/000.mfl` 换掉 0 号布衣的样子),配置无需改动。

## 从原版资源一次性迁移

```
cargo run -p mir-pack --release -- convert resources/Data packs
```

多线程转码引擎注册表内的全部库(1000+ 个,几分钟),已存在的目标文件
跳过 — 所以先打包的自有素材不会被原版转码覆盖。

## 工具

```
cargo run -p mir-pack -- pack <PNG目录> <输出.mfl>    # LibraryEditor 解包目录 → .mfl
cargo run -p mir-pack -- pack-list <输出.mfl> <PNG>...  # 散图按顺序打帧 (立绘等)
cargo run -p mir-pack -- info <xxx.mfl>              # 帧数统计
cargo run -p mir-pack -- preview <xxx.mfl> <out.png> [起始帧] [帧数]   # 验货预览
```

`pack` 输入目录格式:逐帧 `NNNNN.PNG` + `Placements/NNNNN.txt`(两行 =
X/Y 锚点),即市售素材常见的 Crystal LibraryEditor 解包形态。

## 立绘

取图优先级: 穿着衣甲 → `portrait/{外观号}.mfl`(裸模打底 + 展示图叠加)
→ 没有展示图则用该外观的站立帧; 未穿衣甲 → `portrait/naked.mfl`。
`pack-list` 路径带 `@dx,dy` 后缀可写入对位偏移(衣服图相对裸模共享中心
的微调,引擎叠加时应用):

```
mir-pack pack-list packs/portrait/050.mfl 男展示.PNG@3,10 女展示.PNG@6,8
```

市售素材的展示图常按"文件夹顺序×10 + 尾号(1=女 2=男)"编号,打包前先与
站立帧比对确认;对位偏移按内容重心估算后肉眼微调。

## 帧表约定

角色/武器沿用 Crystal FrameSet.Player:站 `0+dir×4`、走 `32+dir×6`、
跑 `80+dir×6`、攻击 `136+dir×6`(dir = 8 方向,0=北顺时针)。

## 版权

`.mfl` 产物与源素材一律**不入 git**(见根 .gitignore);每次添加素材请在
`manifest.md` 登记来源与授权范围。`MIRFORGE_PACKS` 环境变量可改包根路径,
默认为工作目录下 `packs/`。
