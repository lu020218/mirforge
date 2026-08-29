# NPC 系统设计方案

> 现状：NPC 在服务器/客户端/协议中**完全未实现**（代码零引用）；资源已就位
> （`Data/NPC/*.Lib` 共 236 个图库）。本方案覆盖数据模型、服务器、协议、客户端
> 与管理台入口，分三期落地，每期自成闭环可验收。

## 一、资源与渲染事实（Crystal 权威）

- NPC 图库：`Data/NPC/{n}.Lib`（`00.Lib`…，≥100 为三位）。管理台按 `image` 号取库。
- 帧规则（`FrameSet.DefaultNPC`）：站立 = 起始帧 0、**4 帧循环、450ms/帧、无方向偏移**
  （NPC 朝向固定，由素材本身决定）。比怪物/角色简单得多，无需帧表推导。
- 因此客户端渲染 NPC ≈ 渲染一个 4 帧循环的静态精灵 + 头顶名字。
- **朝向字段已去除**：初稿在数据模型与 `npcList` 里带了 `dir`，但既然帧表无方向偏移、
  朝向由素材决定，这个字段永远用不上。留着只会让人以为配了有用，故明确删掉。

## 二、数据模型（SQLite 配置表，与现有配置同库同风格）

```
cfg_npcs         id PK, name, map, x, y, image, kind, enabled, ord
                 kind: talk | shop | quest | teleport
cfg_npc_dialogs  npc_id, page, text                     -- 对话页
cfg_npc_options  npc_id, page, idx, label, action, arg  -- 每页的选项
                 action: page(跳页) | shop | quest_accept | quest_complete
                       | teleport(arg=map:x:y) | close
cfg_npc_shop     npc_id, item_template, price, stock(-1=无限)
```

配套：`cfg_items` 增 `price` 列（商店定价）；`characters` 增 `gold` 列
（金币；HUD 背包底栏已预留"金币"位，现恒为 0）。

校验（服务器唯一校验点，沿用现有 `validate` 模式）：
NPC id 唯一 · 所在地图已接入 · 坐标可走 · 商店引用的物品模板存在 ·
选项跳转的页存在 · 任务动作引用的任务 id 存在 · 传送目标地图已接入。

## 三、协议增量

| 方向 | 消息 | 载荷 |
|---|---|---|
| S→C | `npcList` | 进区/切区下发本区 NPC：id/name/x/y/image |
| C→S | `talkNpc` | npc_id（点击 NPC） |
| S→C | `npcDialog` | npc_id / page / text / options[{idx,label}] |
| C→S | `npcOption` | npc_id / page / idx |
| S→C | `npcShop` | items[{template,name,image,price,stock}] |
| C→S | `buyItem` / `sellItem` | npc_id + template/数量 · item_id |
| S→C | `goldChanged` | gold |

服务器所有交互均校验距离（与 NPC 相距 ≤ `sim::NPC_TALK_RANGE`）与合法性，拒绝越权。
**不校验视线**：隔着房屋、围墙照样能对话，与原版一致；半径本身即是上界。

## 四、管理台入口（本方案重点）

左侧导航「游戏配置」组新增 **NPC 设置**（位于地图设置之后）。

**列表页**：`id / 名称 / 所在地图 / 坐标 / 形象(缩略图) / 类型 / 摘要(商店N件·对话N页) / 启用 / 操作`。
顶部工具条：新增 NPC、按地图筛选、保存、重新载入。

**行内手风琴编辑**（与地图设置同一交互语言）：
1. **基础**：名称、所在地图（下拉，仅已接入区域）、坐标 X/Y、类型、启用开关；
2. **形象**：`image` 输入框 + 实时预览 + **分页图库选择器**（复用物品/小地图那套：
   服务端 `/api/npcs?start&count` 出网格 PNG，`/api/frame/npc/{n}` 出单帧）；
3. **定位**：内嵌地图视口（复用现有瓦片视口组件），显示该地图已有 NPC 标记（紫色），
   「取点」按钮点图直接写入坐标；
4. **对话**：页列表（page + 文本 + 选项子表：标签/动作/参数），动作下拉联动参数框
   （跳页→页号、任务→任务下拉、传送→地图+坐标、商店/关闭→无参）；
5. **商店**（kind=shop 时显示）：物品下拉（取自物品配置）+ 价格 + 库存。

**与地图设置页联动**：地图视口的标记叠加增加 NPC（紫色点，悬停显示名字）；
地图页工具条增加「取点: 新 NPC」，点图后跳到 NPC 设置并预填地图与坐标。

**接口**（沿用 `x-admin-token` 与"校验→写库→热重载"链路）：
`GET/PUT /api/npcs`（全量读写，整表事务替换）、`GET /api/npcs/grid`（图库网格）、
`GET /api/frame/npc/{n}`（单帧预览）。保存后服务器重建本区 NPC 并向在线玩家重推 `npcList`。

## 五、分期

| 期 | 范围 | 验收 |
|---|---|---|
| **P1 存在与展示** ✅ 已完成 | 配置表 + 管理台 NPC 页（列表/基础/形象选择/地图取点）+ 协议 `npcList` + 客户端渲染（4 帧站立 + 名字） | 管理台新增一个 NPC 并保存后，客户端进图即可看到 NPC 站在指定坐标 |
| **P2 对话** ✅ 已完成 | 对话页/选项配置 + `talkNpc`/`npcDialog`/`npcOption` + 客户端对话框 UI（沿用面板族鎏金风格）+ 任务动作接入（接取/交付走现有任务系统） | 点击 NPC 弹出对话，选项可跳页、可接取/交付任务 |
| **P3 商店与金币** | `characters.gold` + `cfg_items.price` + 商店表 + 买卖消息与校验 + 客户端商店界面 + HUD 金币显示 | 从 NPC 买入/卖出装备，金币与背包正确增减并持久化 |

### P1 落地记录

| 环节 | 落点 |
|---|---|
| 配置表 | `cfg_npcs` (`server/src/config_store.rs`)，种子 `server/data/npcs.json` |
| 校验 | `GameData::validate` — id 唯一、名称非空 |
| 管理台 | 侧栏「NPC 设置」页：表格增删改 + 形象图库 (`/api/npcs/grid`) + 「取点」跳地图页左键取坐标 + 地图上紫色 NPC 标记 |
| 协议 | `ServerMessage::NpcList { npcs: Vec<NpcInfo> }` |
| 下发时机 | 进入游戏、切换区域、配置热重载后重推 |
| 客户端 | `npc_step` — `Data/NPC/{image:02}.Lib` 站立 4 帧 @450ms，头顶金色名字，按格 y 参与深度排序 |

### P2 落地记录

| 环节 | 落点 |
|---|---|
| 配置表 | `cfg_npc_dialogs` / `cfg_npc_options`，与 `cfg_npcs` 同事务读写 |
| 校验 | 页号唯一、跳页目标存在、任务动作引用的任务 id 存在、动作名合法 |
| 管理台 | NPC 行「对话」按钮 → 行内手风琴：页号/正文/选项表（标签·动作下拉·参数，参数框随动作联动） |
| 协议 | C→S `talkNpc` / `npcOption`，S→C `npcDialog` / `npcDialogEnd` |
| 服务端 | 交互距离 ≤ `sim::NPC_TALK_RANGE`（8 格，**不做视线判定**）；动作分发 page / quest_accept / quest_complete / close；走远后再点选项明确回 `npcDialogEnd` |
| 客户端 | 左键点 NPC 发 `talkNpc`（不触发走路，够不着先走过去进圈自动开口）；命中按精灵实际渲染矩形判定（整个人都点得中，不是脚下一小格）；鎏金风对话框，标题=NPC 名，选项按钮悬停变金边 |
| 示例数据 | `server/data/npcs.json` 内置比奇守卫两页对话，含接取/交付「新手试炼·猎鸡」 |

补齐项（第二轮）：

| 项 | 落点 |
|---|---|
| `teleport` 动作 | 选项 arg = `地图:x:y`，落点不可走时吸附到最近可走格，复用传送门那套切区下发 |
| 落位校验 | 新增 `AdminCmd::CheckNpcs` —— 走格与区域表只在游戏循环里，故与 `GameData::validate` 分开：地图已接入 / 坐标可站立 / 传送目标地图已接入 / 传送落点可站立 |
| 管理台「摘要」列 | 显示对话页数（商店件数待 P3） |
| 管理台按地图筛选 | 工具条下拉，随区域表刷新 |
| 手风琴「定位」段 | 把唯一的瓦片视口搬进手风琴就地取点（`mountMapView` / `parkMapView`，与 `#zone-edit` 同一套寄存思路），可反复微调不跳页 |
| 地图页 →「取点: 新 NPC」 | 点图后跳 NPC 页并插入预填地图+坐标的新行 |

未做（属 P3）：商店、金币。

## 六、工作量与风险

- P1 ≈ 1 轮：表 + 管理台页 + 协议 + 客户端精灵（渲染规则已明确，风险低）。
- P2 ≈ 1–2 轮：主要成本在客户端对话框 UI 与选项动作分发。
- P3 ≈ 1–2 轮：金币是新增的持久化字段，需覆盖存档/掉落/交易路径，需回归测试。
- 风险点：金币引入后与既有掉落/背包上限逻辑的交叉；对话树的循环引用（校验拦截）。
