//! MirForge 客户端（Bevy）。
//!
//! M1 引导版：窗口 + 2D 相机 + 资源目录探测。
//! 环境变量 `MIRFORGE_RES` 指向传奇资源根目录时，启动即扫描并在日志报告清单
//! （地图渲染为任务 1.2/1.3，见 docs/DEVELOPMENT_PLAN.md）。

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "MirForge".into(),
                ..default()
            }),
            ..default()
        }))
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, windows: Query<&Window>) {
    commands.spawn(Camera2d);
    if let Ok(w) = windows.get_single() {
        info!(
            "MirForge 客户端启动: 窗口 {}x{} 逻辑px, scale_factor {}",
            w.width(),
            w.height(),
            w.scale_factor()
        );
    }
    match std::env::var("MIRFORGE_RES") {
        Ok(root) => {
            let t = std::time::Instant::now();
            let idx = mir_formats::scan::ResourceIndex::scan(std::path::Path::new(&root));
            info!(
                "资源目录 {root}: 地图 {} 张, 图库 {} 个 (可解码 {}), 未识别 {} — 耗时 {:?}",
                idx.maps.len(),
                idx.libs.len(),
                idx.libs.iter().filter(|l| l.supported).count(),
                idx.unknown.len(),
                t.elapsed()
            );
        }
        Err(_) => info!("未设置 MIRFORGE_RES, 跳过资源扫描 (设置后启动即索引资源目录)"),
    }
}
