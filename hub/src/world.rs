//! 地图世界校验 (hub 侧, 无游戏循环)。
//!
//! hub 与区服共享同一份 packs 资源, 因此落位校验 (坐标可走/地图存在)
//! 不必代理给区服: 直接解析 .map 构建与区服判定一致的行走网格
//! (sim::WalkGrid), 全部区服离线时管理台照常可校验保存。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gamedata::defs::{BossDef, GameData, NpcDef, QuestDef, ZoneSidecar};
use sim::{WalkGrid, BODY_RADIUS};

/// 行走网格缓存: 地图名(小写) → 网格。大地图解析不便宜, 进程内长驻
static GRIDS: Mutex<Option<HashMap<String, Arc<WalkGrid>>>> = Mutex::new(None);

/// packs/map 下全部地图: 文件名(小写) → 路径
pub fn maps_index() -> HashMap<String, PathBuf> {
    let root = crate::assets::packs_root().join("map");
    let idx = mir_formats::scan::ResourceIndex::scan(&root);
    idx.maps
        .iter()
        .filter_map(|m| {
            let name = m.path.file_name()?.to_str()?.to_lowercase();
            Some((name, m.path.clone()))
        })
        .collect()
}

pub fn walk_grid(map: &str) -> Option<Arc<WalkGrid>> {
    let key = map.to_lowercase();
    if let Some(g) = GRIDS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .get(&key)
    {
        return Some(g.clone());
    }
    let path = maps_index().remove(&key)?;
    let data = mir_formats::map::parse(&std::fs::read(path).ok()?).ok()?;
    let grid = Arc::new(WalkGrid::from_cells(data.width, data.height, |x, y| {
        data.cell(x, y).is_some_and(|c| c.blocked)
    }));
    GRIDS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key, grid.clone());
    Some(grid)
}

fn parse_teleport(arg: &str) -> Option<(String, f64, f64)> {
    let mut it = arg.split(',').map(str::trim);
    let map = it.next()?.to_lowercase();
    let x = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    Some((map, x, y))
}

/// NPC/BOSS/任务目标落位校验 (与区服 check_*_placement 同规则)
pub fn check_placement(
    zones: &HashMap<String, ZoneSidecar>,
    data: &GameData,
    npcs: &[NpcDef],
    bosses: &[BossDef],
    quests: &[QuestDef],
) -> Vec<String> {
    let mut errs = Vec::new();
    let walkable = |map: &str, x: f64, y: f64| -> Option<bool> {
        walk_grid(map).map(|g| g.is_walkable_circle(x, y, BODY_RADIUS))
    };
    for n in npcs {
        if !zones.contains_key(&n.map.to_lowercase()) {
            errs.push(format!("NPC {} 的地图未接入为区域: {}", n.id, n.map));
            continue;
        }
        if walkable(&n.map, n.x, n.y) == Some(false) {
            errs.push(format!(
                "NPC {} 的坐标不可站立 ({:.1},{:.1})",
                n.id, n.x, n.y
            ));
        }
        for d in &n.dialogs {
            for o in &d.options {
                if o.action != "teleport" {
                    continue;
                }
                match parse_teleport(&o.arg) {
                    Some((map, x, y)) => {
                        if !zones.contains_key(&map) {
                            errs.push(format!(
                                "NPC {} 第 {} 页选项「{}」传送到未接入的地图: {map}",
                                n.id, d.page, o.label
                            ));
                        } else if walkable(&map, x, y) == Some(false) {
                            errs.push(format!(
                                "NPC {} 第 {} 页选项「{}」传送落点不可站立: {map} ({x:.1},{y:.1})",
                                n.id, d.page, o.label
                            ));
                        }
                    }
                    None => errs.push(format!(
                        "NPC {} 第 {} 页选项「{}」传送参数非法: {}",
                        n.id, d.page, o.label, o.arg
                    )),
                }
            }
        }
    }
    for b in bosses {
        if !zones.contains_key(&b.map.to_lowercase()) {
            errs.push(format!("BOSS {} 的地图未接入为区域: {}", b.id, b.map));
            continue;
        }
        if walkable(&b.map, b.x, b.y) == Some(false) {
            errs.push(format!(
                "BOSS {} 的坐标不可站立 ({:.1},{:.1})",
                b.id, b.x, b.y
            ));
        }
    }
    let mut known: std::collections::HashSet<&str> = zones
        .values()
        .flat_map(|z| z.monsters.iter().map(|s| s.template.as_str()))
        .collect();
    known.extend(data.bosses.iter().map(|b| b.id.as_str()));
    for q in quests {
        for (target, need) in &q.objectives {
            if !known.contains(target.as_str()) {
                errs.push(format!(
                    "任务 {} 的击杀目标不存在于任何刷新点或 BOSS: {target}",
                    q.id
                ));
            }
            if *need == 0 {
                errs.push(format!("任务 {} 的目标 {target} 数量不能为 0", q.id));
            }
        }
    }
    errs
}

/// 边车业务校验 (与区服 apply_zone_sidecar 同规则; 地图可解析性一并确认)
pub fn validate_sidecar(
    map: &str,
    sidecar: &ZoneSidecar,
    zones: &HashMap<String, ZoneSidecar>,
    data: &GameData,
) -> Result<(), String> {
    for m in &sidecar.monsters {
        if !data.monsters.iter().any(|md| md.id == m.template) {
            return Err(format!(
                "刷新点引用不存在的怪物模板: {} (先在「怪物设置」里建)",
                m.template
            ));
        }
        for dr in &m.drops {
            if !data.items.iter().any(|i| i.template == dr.item) {
                return Err(format!("掉落引用不存在的物品: {}", dr.item));
            }
        }
    }
    let idx = maps_index();
    for pt in &sidecar.portals {
        let to = pt.to.to_lowercase();
        if !zones.contains_key(&to) && !idx.contains_key(&to) {
            return Err(format!("传送门指向不存在的地图: {}", pt.to));
        }
    }
    if walk_grid(map).is_none() {
        return Err(format!("地图解析失败: {map}"));
    }
    Ok(())
}
