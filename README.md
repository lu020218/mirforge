# MirForge

**开源的现代化传奇（Legend of Mir 2）游戏引擎，Rust 实现，包含服务器与客户端。**

- 🦀 全 Rust：服务器（权威模拟）+ 客户端（[Bevy](https://bevyengine.org)）+ 双端共享的协议与判定 crate
- 📦 **直读原版资源**：把市面上已有的传奇资源目录（`.map` / `WIL/WIX` / `WZL/WZX` / Crystal `.Lib`）指给引擎即可运行，无需预转换
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
server/          权威服务器
client/          Bevy 客户端
```

## 快速开始

引擎唯一资源根是仓库根 `packs/`（图库 `.mfl` + 地图 `.map`,均被 .gitignore
排除,本仓库不含任何游戏资源）。市售素材先用 `mir-pack` 打包/收入
packs（格式与命令见 [packs/README.md](packs/README.md)）；`resources/`
只是素材原始文件的开发态堆场,引擎不读它。`MIRFORGE_PACKS` 可改包根路径。

**Windows (PowerShell) 一键脚本：**

```powershell
# 联机 (两个窗口)
tools\dev-server.ps1                 # 窗口 1: 服务器
tools\dev-client.ps1                 # 窗口 2: 客户端 (注册→建角→进游戏)

# 离线单机漫游 (不需要服务器)
tools\dev-client.ps1 -Offline

# 资源目录不在默认位置时
tools\dev-server.ps1 -Res D:\mir-res
```

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
`MIRFORGE_DB`(SQLite 路径)、`MIRFORGE_LIBSET`(默认 WemadeMir2)。
区域/刷怪/掉落/传送门由 `server/zones/*.json` 边车配置驱动。

服务端自带 Web 管理台 (默认 <http://127.0.0.1:4001>)，物品/技能/NPC/任务/地图/BOSS
均可视化维护、保存即热重载。操作手册见 [docs/ADMIN_GUIDE.md](docs/ADMIN_GUIDE.md)。

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
