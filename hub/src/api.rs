//! hub 专属端点: 配置 CRUD (rev 权威)、区域、公告、区服注册表、
//! 内部通道 (快照/变更推送/心跳)、总览。

use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Html;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::assets::audit;
use crate::state::{authed, hub_authed, AppState};
use gamedata::defs::{GameData, ZoneSidecar};
use gamedata::store;

/// 配置保存成功后的统一收尾: rev+1 并广播给全部区服订阅
async fn bump_and_notify(st: &AppState) -> Result<i64, sqlx::Error> {
    let rev = store::bump_rev(&st.pool).await?;
    let _ = st.cfg_tx.send(rev);
    Ok(rev)
}

// ─────────── 配置 CRUD ───────────

#[derive(Serialize)]
pub(crate) struct ConfigPayload {
    items: serde_json::Value,
    skills: serde_json::Value,
    quests: serde_json::Value,
    npcs: serde_json::Value,
    bosses: serde_json::Value,
    monsters: serde_json::Value,
}

pub(crate) async fn api_config_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ConfigPayload>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let d = store::load_game_data(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(ConfigPayload {
        items: serde_json::to_value(&d.items).unwrap_or_default(),
        skills: serde_json::to_value(&d.skills).unwrap_or_default(),
        quests: serde_json::to_value(&d.quests).unwrap_or_default(),
        npcs: serde_json::to_value(&d.npcs).unwrap_or_default(),
        bosses: serde_json::to_value(&d.bosses).unwrap_or_default(),
        monsters: serde_json::to_value(&d.monsters).unwrap_or_default(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct PutConfigReq {
    kind: String,
    value: serde_json::Value,
}

#[derive(Serialize)]
pub(crate) struct PutConfigResp {
    ok: bool,
    errors: Vec<String>,
}

pub(crate) async fn api_config_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PutConfigReq>,
) -> Result<Json<PutConfigResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let fail = |e: String| {
        Json(PutConfigResp {
            ok: false,
            errors: vec![e],
        })
    };
    let mut next: GameData = match store::load_game_data(&st.pool).await {
        Ok(d) => d,
        Err(e) => return Ok(fail(format!("读取当前配置失败: {e}"))),
    };
    let parsed: Result<(), String> = (|| {
        match req.kind.as_str() {
            "items" => {
                next.items =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("items: {e}"))?
            }
            "skills" => {
                next.skills =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("skills: {e}"))?
            }
            "quests" => {
                next.quests =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("quests: {e}"))?
            }
            "npcs" => {
                next.npcs =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("npcs: {e}"))?
            }
            "bosses" => {
                next.bosses =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("bosses: {e}"))?
            }
            "monsters" => {
                next.monsters = serde_json::from_value(req.value.clone())
                    .map_err(|e| format!("monsters: {e}"))?
            }
            k => return Err(format!("未知配置类别: {k}")),
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return Ok(fail(e));
    }
    let mut errors = next.validate();
    if matches!(req.kind.as_str(), "npcs" | "bosses" | "quests") && errors.is_empty() {
        let zones = store::load_zone_sidecars(&st.pool)
            .await
            .unwrap_or_default();
        errors.extend(crate::world::check_placement(
            &zones,
            &next,
            &next.npcs,
            &next.bosses,
            &next.quests,
        ));
    }
    if !errors.is_empty() {
        return Ok(Json(PutConfigResp { ok: false, errors }));
    }
    let saved = match req.kind.as_str() {
        "items" => store::save_items(&st.pool, &next.items).await,
        "skills" => store::save_skills(&st.pool, &next.skills).await,
        "npcs" => store::save_npcs(&st.pool, &next.npcs).await,
        "bosses" => store::save_bosses(&st.pool, &next.bosses).await,
        "monsters" => store::save_monsters(&st.pool, &next.monsters).await,
        _ => store::save_quests(&st.pool, &next.quests).await,
    };
    if let Err(e) = saved {
        return Ok(fail(format!("保存失败: {e}")));
    }
    if let Err(e) = bump_and_notify(&st).await {
        return Ok(fail(format!("版本号更新失败: {e}")));
    }
    audit("put_config", &req.kind);
    Ok(Json(PutConfigResp {
        ok: true,
        errors: Vec::new(),
    }))
}

// ─────────── 区域 (zones) ───────────

#[derive(Serialize)]
pub(crate) struct ZoneRow {
    map: String,
    name: String,
    sidecar: serde_json::Value,
}

#[derive(Serialize)]
pub(crate) struct ZonesInfo {
    zones: Vec<ZoneRow>,
    available: Vec<String>,
}

pub(crate) async fn api_zones_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ZonesInfo>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let sidecars = store::load_zone_sidecars(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut zones: Vec<ZoneRow> = sidecars
        .iter()
        .map(|(map, sc)| ZoneRow {
            map: map.clone(),
            name: sc.name.clone().unwrap_or_else(|| map.clone()),
            sidecar: serde_json::to_value(sc).unwrap_or_default(),
        })
        .collect();
    zones.sort_by(|a, b| a.map.cmp(&b.map));
    let mut available: Vec<String> = crate::world::maps_index()
        .into_keys()
        .filter(|m| !sidecars.contains_key(m))
        .collect();
    available.sort();
    Ok(Json(ZonesInfo { zones, available }))
}

#[derive(Deserialize)]
pub(crate) struct PutZoneReq {
    map: String,
    sidecar: serde_json::Value,
}

#[derive(Deserialize)]
pub(crate) struct AddZoneReq {
    map: String,
}

#[derive(Serialize)]
pub(crate) struct ZoneOpResp {
    ok: bool,
    error: Option<String>,
}

fn zone_resp(r: Result<(), String>) -> Json<ZoneOpResp> {
    match r {
        Ok(()) => Json(ZoneOpResp {
            ok: true,
            error: None,
        }),
        Err(e) => Json(ZoneOpResp {
            ok: false,
            error: Some(e),
        }),
    }
}

async fn put_zone_inner(st: &AppState, map: &str, value: serde_json::Value) -> Result<(), String> {
    let map = map.to_lowercase();
    let sidecar: ZoneSidecar =
        serde_json::from_value(value).map_err(|e| format!("边车解析失败: {e}"))?;
    let zones = store::load_zone_sidecars(&st.pool)
        .await
        .map_err(|e| format!("读取区域失败: {e}"))?;
    if !zones.contains_key(&map) {
        return Err(format!("区域未接入: {map}"));
    }
    let data = store::load_game_data(&st.pool)
        .await
        .map_err(|e| format!("读取配置失败: {e}"))?;
    crate::world::validate_sidecar(&map, &sidecar, &zones, &data)?;
    store::save_zone(&st.pool, &map, &sidecar)
        .await
        .map_err(|e| format!("保存区域失败: {e}"))?;
    bump_and_notify(st)
        .await
        .map_err(|e| format!("版本号更新失败: {e}"))?;
    Ok(())
}

pub(crate) async fn api_zones_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PutZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let r = put_zone_inner(&st, &req.map, req.sidecar).await;
    if r.is_ok() {
        audit("put_zone", &req.map);
    }
    Ok(zone_resp(r))
}

pub(crate) async fn api_zones_add(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let map = req.map.to_lowercase();
    let r: Result<(), String> = async {
        let zones = store::load_zone_sidecars(&st.pool)
            .await
            .map_err(|e| format!("读取区域失败: {e}"))?;
        if zones.contains_key(&map) {
            return Err(format!("区域已存在: {map}"));
        }
        if crate::world::walk_grid(&map).is_none() {
            return Err(format!("资源目录中没有该地图或解析失败: {map}"));
        }
        store::save_zone(&st.pool, &map, &ZoneSidecar::default())
            .await
            .map_err(|e| format!("保存区域失败: {e}"))?;
        bump_and_notify(&st)
            .await
            .map_err(|e| format!("版本号更新失败: {e}"))?;
        Ok(())
    }
    .await;
    if r.is_ok() {
        audit("add_zone", &map);
    }
    Ok(zone_resp(r))
}

pub(crate) async fn api_zones_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let map = req.map.to_lowercase();
    let r: Result<(), String> = async {
        let zones = store::load_zone_sidecars(&st.pool)
            .await
            .map_err(|e| format!("读取区域失败: {e}"))?;
        if !zones.contains_key(&map) {
            return Err(format!("区域不存在: {map}"));
        }
        let default_zone = std::env::var("MIRFORGE_MAP").unwrap_or_else(|_| "0.map".into());
        if map == default_zone.to_lowercase() {
            return Err("缺省出生区域不可移除".into());
        }
        let refs: Vec<String> = zones
            .iter()
            .filter(|(m, sc)| *m != &map && sc.portals.iter().any(|p| p.to.to_lowercase() == map))
            .map(|(m, sc)| sc.name.clone().unwrap_or_else(|| m.clone()))
            .collect();
        if !refs.is_empty() {
            return Err(format!("以下区域的传送门指向它: {}", refs.join("、")));
        }
        store::delete_zone(&st.pool, &map)
            .await
            .map_err(|e| format!("删除区域失败: {e}"))?;
        bump_and_notify(&st)
            .await
            .map_err(|e| format!("版本号更新失败: {e}"))?;
        Ok(())
    }
    .await;
    if r.is_ok() {
        audit("delete_zone", &map);
    }
    Ok(zone_resp(r))
}

// ─────────── 公告 (全局; 管理保存 → WS 实时推送) ───────────

pub(crate) async fn api_news_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<store::NewsItem>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    store::load_news(&st.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn api_news_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(items): Json<Vec<store::NewsItem>>,
) -> Result<StatusCode, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    audit("news", &format!("{} 条公告", items.len()));
    store::save_news(&st.pool, &items)
        .await
        .map(|_| {
            let _ = st.news_tx.send(());
            StatusCode::NO_CONTENT
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) async fn api_public_news(
    State(st): State<AppState>,
) -> Result<Json<Vec<store::NewsItem>>, StatusCode> {
    let mut items = store::load_news(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    items.truncate(20);
    Ok(Json(items))
}

/// 公告: 登录器 WS 订阅 (无鉴权)。连上先推一次全量列表,
/// 之后管理台每次保存都会实时推送最新列表; 30 秒 ping 保活。
pub(crate) async fn api_public_news_ws(
    State(st): State<AppState>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> axum::response::Response {
    ws.on_upgrade(move |sock| news_ws_loop(sock, st))
}

async fn news_ws_loop(mut sock: axum::extract::ws::WebSocket, st: AppState) {
    use axum::extract::ws::Message;
    async fn latest(st: &AppState) -> Option<String> {
        let mut items = store::load_news(&st.pool).await.ok()?;
        items.truncate(20);
        serde_json::to_string(&items).ok()
    }
    let mut rx = st.news_tx.subscribe();
    match latest(&st).await {
        Some(json) => {
            if sock.send(Message::Text(json)).await.is_err() {
                return;
            }
        }
        None => return,
    }
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
    ping.tick().await;
    loop {
        tokio::select! {
            r = rx.recv() => {
                if matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return;
                }
                let Some(json) = latest(&st).await else { return };
                if sock.send(Message::Text(json)).await.is_err() {
                    return;
                }
            }
            _ = ping.tick() => {
                if sock.send(Message::Ping(Vec::new())).await.is_err() {
                    return;
                }
            }
            m = sock.recv() => {
                match m {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

// ─────────── 区服注册表 ───────────

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ServerRow {
    pub id: String,
    pub name: String,
    /// 玩家连接的游戏服地址 (ws://…)
    pub game: String,
    /// 区服内部运行时 API (hub 代理踢人/广播用; 可空)
    #[serde(default)]
    pub internal: String,
    #[serde(default)]
    pub sort: i64,
    #[serde(default)]
    pub hidden: bool,
}

pub(crate) async fn load_servers(pool: &sqlx::SqlitePool) -> Result<Vec<ServerRow>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, name, game_addr, internal_addr, sort, hidden FROM servers ORDER BY sort, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| ServerRow {
            id: r.get(0),
            name: r.get(1),
            game: r.get(2),
            internal: r.get(3),
            sort: r.get(4),
            hidden: r.get::<i64, _>(5) != 0,
        })
        .collect())
}

pub(crate) async fn api_servers_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ServerRow>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    load_servers(&st.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// 全量替换注册表 (与配置页同风格: 前端整表提交)
pub(crate) async fn api_servers_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(rows): Json<Vec<ServerRow>>,
) -> Result<StatusCode, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let mut tx = st
        .pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    sqlx::query("DELETE FROM servers")
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    for r in &rows {
        if r.id.trim().is_empty() || r.name.trim().is_empty() {
            return Err(StatusCode::BAD_REQUEST);
        }
        sqlx::query(
            "INSERT INTO servers (id, name, game_addr, internal_addr, sort, hidden)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&r.id)
        .bind(&r.name)
        .bind(&r.game)
        .bind(&r.internal)
        .bind(r.sort)
        .bind(i64::from(r.hidden))
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    audit("servers", &format!("{} 个区服", rows.len()));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub(crate) struct PublicServer {
    id: String,
    name: String,
    game: String,
}

/// 登录器: 区服列表 (隐藏项不出)
pub(crate) async fn api_public_servers(
    State(st): State<AppState>,
) -> Result<Json<Vec<PublicServer>>, StatusCode> {
    let rows = load_servers(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        rows.into_iter()
            .filter(|r| !r.hidden)
            .map(|r| PublicServer {
                id: r.id,
                name: r.name,
                game: r.game,
            })
            .collect(),
    ))
}

// ─────────── 内部通道 (区服 ↔ hub) ───────────

/// 全量配置快照 (区服启动/热更拉取)
pub(crate) async fn api_internal_snapshot(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<gamedata::defs::Snapshot>, StatusCode> {
    if !hub_authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    store::load_snapshot(&st.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
pub(crate) struct WsTokenQuery {
    #[serde(default)]
    token: String,
}

/// 配置变更订阅: 连上先推当前 rev, 之后每次保存推新 rev
/// (鉴权走 ?token= — 区服 WS 客户端带自定义头不便)
pub(crate) async fn api_internal_config_ws(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<WsTokenQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(t) = &st.hub_token {
        if q.token != *t {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(ws.on_upgrade(move |sock| cfg_ws_loop(sock, st)))
}

async fn cfg_ws_loop(mut sock: axum::extract::ws::WebSocket, st: AppState) {
    use axum::extract::ws::Message;
    let mut rx = st.cfg_tx.subscribe();
    let rev = store::get_rev(&st.pool).await.unwrap_or(0);
    if sock
        .send(Message::Text(format!("{{\"rev\":{rev}}}")))
        .await
        .is_err()
    {
        return;
    }
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
    ping.tick().await;
    loop {
        tokio::select! {
            r = rx.recv() => {
                let rev = match r {
                    Ok(rev) => rev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        store::get_rev(&st.pool).await.unwrap_or(0)
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };
                if sock.send(Message::Text(format!("{{\"rev\":{rev}}}"))).await.is_err() {
                    return;
                }
            }
            _ = ping.tick() => {
                if sock.send(Message::Ping(Vec::new())).await.is_err() {
                    return;
                }
            }
            m = sock.recv() => {
                match m {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct HeartbeatReq {
    server: String,
    rev: i64,
    online: u32,
}

pub(crate) async fn api_internal_heartbeat(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<HeartbeatReq>,
) -> Result<StatusCode, StatusCode> {
    if !hub_authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    st.beats.lock().unwrap().insert(
        req.server,
        crate::state::Heartbeat {
            rev: req.rev,
            online: req.online,
            at: std::time::Instant::now(),
        },
    );
    Ok(StatusCode::NO_CONTENT)
}

// ─────────── 总览 (注册表 × 心跳 × rev 一致性) ───────────

#[derive(Serialize)]
pub(crate) struct OverviewRow {
    id: String,
    name: String,
    game: String,
    /// 最近心跳距今秒数 (None = 从未上报)
    beat_secs: Option<u64>,
    rev: Option<i64>,
    online: Option<u32>,
    /// rev 与 hub 一致
    fresh: bool,
}

#[derive(Serialize)]
pub(crate) struct Overview {
    rev: i64,
    servers: Vec<OverviewRow>,
}

pub(crate) async fn api_overview(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Overview>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let rev = store::get_rev(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let rows = load_servers(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let beats = st.beats.lock().unwrap().clone();
    let servers = rows
        .into_iter()
        .map(|r| {
            let b = beats.get(&r.id);
            OverviewRow {
                beat_secs: b.map(|b| b.at.elapsed().as_secs()),
                rev: b.map(|b| b.rev),
                online: b.map(|b| b.online),
                fresh: b.is_some_and(|b| b.rev == rev),
                id: r.id,
                name: r.name,
                game: r.game,
            }
        })
        .collect();
    Ok(Json(Overview { rev, servers }))
}

// ─────────── 地图缩略/瓦片 (路径与覆盖点来自 hub 自身) ───────────

pub(crate) async fn api_map_thumb(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(map): AxPath<String>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let map = map.to_lowercase();
    let path = crate::world::maps_index()
        .remove(&map)
        .ok_or(StatusCode::NOT_FOUND)?;
    let sc = store::load_zone_sidecars(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .remove(&map)
        .unwrap_or_default();
    let out = tokio::task::spawn_blocking(move || {
        let mapdata = mir_formats::map::parse(&std::fs::read(&path).ok()?).ok()?;
        let (mw, mh) = (mapdata.width, mapdata.height);
        let step = ((mw.max(mh) as f32 / 512.0).ceil() as u32).max(1);
        let (tw, th) = (mw / step, mh / step);
        let mut img = image::RgbaImage::new(tw.max(1), th.max(1));
        for ty in 0..th {
            for tx2 in 0..tw {
                let (mut walk, mut total) = (0u32, 0u32);
                for dy in 0..step {
                    for dx in 0..step {
                        if let Some(c) = mapdata.cell(tx2 * step + dx, ty * step + dy) {
                            total += 1;
                            if !c.blocked {
                                walk += 1;
                            }
                        }
                    }
                }
                let v = (walk * 255).checked_div(total).unwrap_or(0);
                let g = 40 + (v * 150 / 255) as u8;
                img.put_pixel(tx2, ty, image::Rgba([g, g, (g as u16 + 14) as u8, 255]));
            }
        }
        let mut mark = |x: f64, y: f64, c: [u8; 4], r: i32| {
            let (px, py) = ((x as u32 / step) as i32, (y as u32 / step) as i32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let (mx, my) = (px + dx, py + dy);
                    if mx >= 0 && my >= 0 && (mx as u32) < tw && (my as u32) < th {
                        img.put_pixel(mx as u32, my as u32, image::Rgba(c));
                    }
                }
            }
        };
        for m in &sc.monsters {
            mark(m.x, m.y, [224, 64, 64, 255], 1);
        }
        for p in &sc.portals {
            mark(p.x, p.y, [90, 220, 220, 255], 2);
        }
        let spawn = sc.spawn.unwrap_or((mw as f64 / 2.0, mh as f64 / 2.0));
        mark(spawn.0, spawn.1, [255, 216, 118, 255], 3);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
        Some((buf.into_inner(), mw, mh))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (png, mw, mh) = out.ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("x-map-width", mw.to_string())
        .header("x-map-height", mh.to_string())
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 地图原图瓦片: 每块 16x16 格 (与区服后台同实现, 路径查询本地化)
pub(crate) async fn api_map_tile(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((map, tx, ty)): AxPath<(String, u32, u32)>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let map_name = map.to_lowercase();
    let path = crate::world::maps_index()
        .remove(&map_name)
        .ok_or(StatusCode::NOT_FOUND)?;
    let png = tokio::task::spawn_blocking(move || {
        let mapdata = mir_formats::map::parse(&std::fs::read(&path).ok()?).ok()?;
        Some(crate::map_render::tile_cached(&map_name, &mapdata, tx, ty))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=86400")
        .body(axum::body::Body::from(png.to_vec()))
        .unwrap())
}

pub(crate) async fn index() -> Html<&'static str> {
    Html(include_str!("../../server/src/admin.html"))
}

// ─────────── 运行时操作代理 (hub → 区服内部 API) ───────────

/// 按服代理: /srv/{id}/api/{path} → 注册表 internal 地址 + 密钥。
/// 只放行运行时操作白名单, 避免 hub 变成任意转发器。
pub(crate) async fn api_srv_proxy(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((id, path)): AxPath<(String, String)>,
    method: axum::http::Method,
    body: axum::body::Bytes,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    const ALLOWED: [&str; 4] = ["status", "broadcast", "kick", "save"];
    if !ALLOWED.contains(&path.as_str()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let servers = load_servers(&st.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let target = servers
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if target.internal.is_empty() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let url = format!("{}/api/{path}", target.internal.trim_end_matches('/'));
    let token = st.hub_token.clone().unwrap_or_default();
    let is_get = method == axum::http::Method::GET;
    let out = tokio::task::spawn_blocking(move || {
        let req = if is_get {
            ureq::get(&url)
        } else {
            ureq::post(&url)
        }
        .set("x-admin-token", &token)
        .set("content-type", "application/json")
        .timeout(std::time::Duration::from_secs(8));
        let resp = if is_get {
            req.call()
        } else {
            req.send_bytes(&body)
        };
        match resp {
            Ok(r) => {
                let status = r.status();
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut r.into_reader(), &mut buf);
                Ok((status, buf))
            }
            Err(ureq::Error::Status(code, r)) => {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut r.into_reader(), &mut buf);
                Ok((code, buf))
            }
            Err(e) => Err(format!("{e}")),
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match out {
        Ok((status, buf)) => Ok(axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(buf))
            .unwrap()),
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}
