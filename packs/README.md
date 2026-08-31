# packs/ — 自有资源包体系

引擎的**唯一**资源根:图库 (`.mfl`) 与地图 (`.map`) 都存放于此。
Crystal 资源路线已废除 — 引擎不再读取任何 `.Lib`,也不读 `resources/`
(那只是素材原始文件的开发态堆场)。素材用 `mir-pack` 打包/收入:
`pack`/`pack-list`/`pack-wil`/`pack-wzl` 打图库,`import-maps` 收地图,
同号文件即替换。

## 目录结构(按类型,不按购买批次)

```
packs/
  items.mfl           物品图标库 (整库, 帧号 = 物品表"图标帧"字段)
  magicon.mfl         技能图标库 (帧号 = 客户端 magicon_index 固定表,
                      图标由特效代表帧裁切生成)
  mmap.mfl            小地图库 (帧号 = 区域配置的小地图帧)
  magic/000.mfl       技能特效 (000=Magic, 001=Magic2)
  weapon/050.mfl      武器外观 (对应物品表"外观"字段, 自有素材建议从 050 起编号)
  armor/050.mfl       衣甲外观
  hair/               发型
  monster/            怪物 (素材包 Mon 号 → 同号)
  npc/                NPC (合集库按 60 帧/个切分, 帧 0-3 = 站立循环)
  map/<套>/<名>.mfl   地图图库 (保持原相对路径, 如 map/WemadeMir2/Tiles.mfl)
  portrait/050.mfl    人物面板立绘 (帧 0=男 1=女, 编号对应衣甲外观号)
  portrait/naked.mfl  裸模立绘 (未穿衣甲时用, 帧 0=男 1=女)
  manifest.md         来源与授权登记 (人工维护)
```

编号即文件名(三位十进制)。要**替换**某个外观,放同号 .mfl 顶掉即可
(如重打 `armor/000.mfl` 换掉 0 号布衣的样子),配置无需改动。

## 从购买素材批量打包

素材包常见形态与对应命令:
- LibraryEditor 解包目录 (逐帧 PNG/BMP + Placements) → `mir-pack pack`
  (BMP 无 alpha, 纯黑自动抠透明)
- NPC 合集帧目录 (每 60 帧一个 NPC) → `mir-pack pack-split <目录> <输出目录> 60`
- 散图 (立绘/小地图) → `mir-pack pack-list`
- Crystal .Lib → `mir-pack convert` (整目录) 或逐个 pack
- WIL/WIX (#ILIB v1.0 16bpp) → `mir-pack pack-wil`
- WZL/WZX (www.shandagames.com 头, enc5=RGB565/enc6=BGR888) → `mir-pack pack-wzl`
  (enc3 调色板与 enc7 变体样本占比 ~5%, 暂按空帧跳过)

地图接入三件套: `.map` 用 `mir-pack import-maps <素材目录> packs` 收入
`packs/map/` (同名只收第一份);
该图的 Tiles/SmTiles/Objects 套打进 `packs/map/`;
小地图图片追加进 `mmap.mfl` 并在管理台把区域的小地图帧号指过去。
`mir-pack mapinfo <xx.map>` 可查看一张图引用的库号
(盛大格式: 层基址 back=100/mid=110/front=120, 文件后缀 = 值-基址+1)。

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

**角色/武器**(衣甲与武器库同布局,实测自购买素材裸模全库分段):
每方向 8 帧位,dir = 8 方向 0=北顺时针;站 `0+dir×8`(4 帧)、
走 `64+dir×8`(6 帧)、跑 `128+dir×8`(6 帧)、攻击 `192+dir×8`(6 帧)、
被击 `480+dir×8`(3 帧)、死亡 `536+dir×8`(4 帧);**女版整段 +600**。

**怪物**:市售库每库基址/跨度不一(有的从 440 帧起、跨度 8 或 10),
引擎自适应:探测首实帧为基址、方向块跨度,按
`基址 + 动作序号×跨度×8 + 方向×跨度` 寻址(动作序:站/走/攻/被击/死),
动画相位按块内实帧数取模(不会踩到空帧位闪烁),缺段回退站立。
一个库含多只怪时,在管理台「怪物设置」给模板配**基址**(该怪的起始帧,
「选段」按钮可视化挑),引擎从基址起自适应。

## 版权

`.mfl` 产物与源素材一律**不入 git**(见根 .gitignore);每次添加素材请在
`manifest.md` 登记来源与授权范围。`MIRFORGE_PACKS` 环境变量可改包根路径,
默认为工作目录下 `packs/`。
