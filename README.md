# MirForge

**开源的现代化传奇（Legend of Mir 2）游戏引擎，Rust 实现，包含服务器与客户端。**

- 🦀 全 Rust：服务器（权威模拟）+ 客户端（[Bevy](https://bevyengine.org)）+ 双端共享的协议与判定 crate
- 📦 **自有资源体系**：市售素材（逐帧 PNG / `WIL/WIX` / `WZL/WZX` / Crystal `.Lib`）经 `mir-pack` 一次打包为自有 `.mfl` 格式，引擎只读 `packs/`
- 🖥️ 现代体验：现代 MMORPG 风格界面、HiDPI/4K 自适应、任意窗口尺寸
- ⚖️ 双许可：MIT OR Apache-2.0

> ⚠️ **本仓库不含任何游戏资源。** 传奇美术资源版权归其权利方所有，
> 使用者需自备合法取得的资源文件。引擎只负责读取与呈现。

## 状态

早期开发中（M0：资源格式解码）。路线图见
[docs/DEVELOPMENT_PLAN.md](docs/DEVELOPMENT_PLAN.md)，
技术方案见 [docs/ENGINE_REBOOT.md](docs/ENGINE_REBOOT.md)。

## 仓库结构

```
crates/
  mir-formats    原版格式解码 (.map / WIL/WIX / WZL/WZX / .Lib)
  mir-atlas      运行时纹理图集 + 磁盘缓存
  protocol       客户端/服务器消息定义 (双端共享)
  sim            移动/碰撞判定 (双端共享, 预测与权威同源)
  gamedata       游戏内容配置的类型与 SQLite 存取 (hub 与区服共用)
hub/             中心站: 配置权威 + 统一管理后台 + 公告/更新/区服列表
server/          权威服务器 (区服; 单机模式可独立跑)
client/          Bevy 客户端
launcher/        登录器 (账号/区服选择/公告/游戏更新)
```

## 快速开始

引擎唯一资源根是仓库根 `packs/`（图库 `.mfl` + 地图 `.map`,均被 .gitignore
排除,本仓库不含任何游戏资源）。市售素材先用 `mir-pack` 打包/收入
packs（格式与命令见 [packs/README.md](packs/README.md)）；`resources/`
只是素材原始文件的开发态堆场,引擎不读它。`MIRFORGE_PACKS` 可改包根路径。

**Windows (PowerShell) 一键脚本：**

```powershell
# 单机开发 (两个窗口; 服务器自带配置库与完整管理台 4001)
tools\dev-server.ps1                 # 窗口 1: 服务器
tools\dev-launcher.ps1               # 窗口 2: 登录器 (登录/注册/更新 → 拉起客户端)

# 多区服形态 (hub 统一后台; 三个窗口起步)
tools\dev-hub.ps1 -Import target\dev.db   # 窗口 1: 中心站 (首次可导入旧单机库配置)
tools\dev-server.ps1 -Hub http://127.0.0.1:4001 -ServerId s1
tools\dev-launcher.ps1                     # 登录器 site 指向 hub, 区服列表自动拉取
# 同机第二区服:
#   tools\dev-server.ps1 -Hub http://127.0.0.1:4001 -ServerId s2 -Addr 127.0.0.1:4010 -Db target/dev2.db -Internal 127.0.0.1:4012
# 起服后在管理台「区服管理」页登记 id/名称/game 地址, 登录器即刻可见
```

客户端不再内置登录界面 —— 账号与更新统一走登录器。
自动化/无登录器直连客户端用环境变量: `MIRFORGE_SERVER` +
`MIRFORGE_AUTOLOGIN=user:pass[:class]` (dev 钩子, 详见 client 源码)。

**bash / CI：**

```bash
cargo run -p mirforge-server                                       # 服务器
MIRFORGE_SERVER=ws://127.0.0.1:4000 cargo run -p mirforge-client   # 客户端
cargo run -p mirforge-client                                       # 或离线单机
```

**游戏内操作**：左键按住空地 = 走路，右键 = 跑步，左键点怪 = 普攻，`1/2/3` = 技能，
`B` 背包 / `C` 装备 / `L` 任务 / `F3` 调试面板，`F` = 1x/2x/3x 整数缩放。

**测试与回归：**

```bash
cargo test --workspace                # 单元/逻辑测试 (不需要资源)
tools/run-smokes.sh                   # 五套协议级端到端冒烟 (需要 packs/)
```

常用环境变量：`MIRFORGE_MAP`(默认 0.map)、`MIRFORGE_ADDR`(默认 127.0.0.1:4000)、
`MIRFORGE_DB`(SQLite 路径)、`MIRFORGE_PACKS`(资源包根, 默认 packs/)。
区域/怪物/物品等全部配置存于 SQLite, 在管理台可视化维护。

Web 管理台 (默认 <http://127.0.0.1:4001>)，物品/技能/NPC/任务/地图/BOSS
均可视化维护、保存即热重载。操作手册见 [docs/ADMIN_GUIDE.md](docs/ADMIN_GUIDE.md)。

## 多区服架构 (hub 中心站)

游戏内容配置(技能/物品/怪物/任务/区域)、公告、客户端更新包与区服列表的
唯一权威在 **hub** (`mirforge-hub`); 区服 (`mirforge-server`) 只存玩家数据。

- **配置流**: 管理台保存 → hub 落库 rev+1 → WS 推给全部区服 → 区服拉全量
  快照免重启热应用 → 心跳回报 rev, 总览页全绿即全区一致。
- **兜底**: 区服把最近快照缓存在 `<db>.snapshot.json`; hub 宕机不影响运行,
  重启区服也能凭缓存开服, hub 恢复后自动对齐。
- **区服环境变量**: `MIRFORGE_HUB` (hub 地址; 不设 = 单机模式旧行为) ·
  `MIRFORGE_SERVER_ID` (注册表 id) · `MIRFORGE_HUB_TOKEN` (内部通道密钥,
  跨机部署必设) · `MIRFORGE_ADMIN` (内部运行时 API, hub 模式默认 4002)。
- **hub 环境变量**: `MIRFORGE_HUB_ADDR` (默认 4001) · `MIRFORGE_HUB_DB` ·
  `MIRFORGE_ADMIN_TOKEN` (运营鉴权) · `MIRFORGE_HUB_TOKEN` ·
  `MIRFORGE_UPDATES` (更新包目录)。
- **新增区服三步**: 起进程 (`-Hub ... -ServerId sN` + 独立端口/库) →
  管理台「区服管理」登记一行 → 登录器自动看到 (无需重发配置)。
- **登录器**: `servers.json` 只需 `{"site": "http://hub:4001"}`;
  区服列表启动时拉取并缓存 (`servers_cache.json`) 供断网兜底。
- **账号按区服隔离** (各服独立玩家库); 统一账号中心是后续课题。

客户端还有三个开发钩子，配合起来可以无人值守地截图自查界面：

| 变量 | 作用 |
|---|---|
| `MIRFORGE_AUTOLOGIN=用户名:密码[:职业]` | 自动 注册→登录→建角→选角 进图 |
| `MIRFORGE_PANELS=bclm` | 进图后自动展开背包/角色/任务面板与大地图 |

大地图 (M) 上左键点一处即自动寻路跑过去; 路线会绕开附近的怪与 NPC, 途中
情况有变会自行重算; 期间在世界里点任意一下即接管停下。
| `MIRFORGE_SHOT=路径[,延迟秒]` | 延迟后截图存盘并退出 |
| `MIRFORGE_PATHDBG=1` | 打印寻路日志: 避让前后的拐点数、途中重算与卡住补救 |
| `MIRFORGE_TALK=npc_id[:选项下标]` | 进图后自动与该 NPC 搭话并选一项（对话框/商店窗自查用） |

## 许可

MIT OR Apache-2.0，任选其一。贡献即表示同意以此双许可发布。
