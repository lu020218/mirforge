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

## 快速开始（占位，随里程碑完善）

```bash
cargo test          # 运行全部测试
```

## 许可

MIT OR Apache-2.0，任选其一。贡献即表示同意以此双许可发布。
