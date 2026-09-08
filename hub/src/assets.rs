//! packs 资源预览与更新包分发 (自 server/src/admin.rs 平移; 仅依赖
//! 管理鉴权与本地只读资源, 无游戏循环)。

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::{authed, AppState};

// ── 资源帧预览 (图标/外观选择器) ──

static PREVIEW_LIBS: std::sync::Mutex<
    Option<std::collections::HashMap<String, Option<mir_formats::mfl::AnyLib>>>,
> = std::sync::Mutex::new(None);

/// 自有资源包根 (与客户端同规则: MIRFORGE_PACKS 可覆盖, 默认工作目录 packs/)
pub(crate) fn packs_root() -> std::path::PathBuf {
    std::env::var("MIRFORGE_PACKS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("packs"))
}

/// 预览库白名单 (packs/ 内固定路径, 杜绝任意文件读取)。
/// Crystal 资源路线已废除, 预览只走 packs — mir-pack convert 负责转码。
fn preview_pack_path(kind: &str, n: u16) -> Option<std::path::PathBuf> {
    let rel = match kind {
        "items" => "items.mfl".to_string(),
        "weapon" => format!("weapon/{n:03}.mfl"),
        "armour" => format!("armor/{n:03}.mfl"),
        "monster" => format!("monster/{n:03}.mfl"),
        "minimap" => "mmap.mfl".to_string(),
        "npc" => format!("npc/{n:03}.mfl"),
        _ => return None,
    };
    Some(packs_root().join(rel))
}

fn with_preview_lib<R>(
    kind: &str,
    n: u16,
    f: impl FnOnce(&mir_formats::mfl::AnyLib) -> Option<R>,
) -> Option<R> {
    let key = format!("{kind}/{n}");
    let mut guard = PREVIEW_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(Default::default);
    if !cache.contains_key(&key) {
        let lib = preview_pack_path(kind, n).and_then(|p| mir_formats::mfl::AnyLib::open(&p).ok());
        cache.insert(key.clone(), lib);
    }
    cache.get(&key).and_then(|l| l.as_ref()).and_then(f)
}

#[derive(Deserialize)]
pub(crate) struct IconsQuery {
    #[serde(default)]
    start: usize,
    #[serde(default = "default_icon_count")]
    count: usize,
}

fn default_icon_count() -> usize {
    100
}

/// 图标网格 PNG: 10 列 × 48px 单元, 棋盘底; 前端按坐标换算帧号
pub(crate) async fn api_icons_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let count = q.count.min(400);
    let cols = 10usize;
    let cell = 48u32;
    let rows = count.div_ceil(cols).max(1);
    let png = tokio::task::spawn_blocking(move || {
        let mut canvas = image::RgbaImage::new(cols as u32 * cell, rows as u32 * cell);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        with_preview_lib("items", 0, |lib| {
            for i in 0..count {
                let Ok(Some(img)) = lib.image(q.start + i) else {
                    continue;
                };
                let (ox, oy) = (((i % cols) as u32) * cell, ((i / cols) as u32) * cell);
                // 居中放置, 超出裁剪
                let (w, h) = (img.width as u32, img.height as u32);
                let dx = ox + cell.saturating_sub(w) / 2;
                let dy = oy + cell.saturating_sub(h) / 2;
                for y in 0..h.min(cell) {
                    for x in 0..w.min(cell) {
                        let si = ((y * w + x) * 4) as usize;
                        let px = &img.rgba[si..si + 4];
                        if px[3] > 0 && dx + x < canvas.width() && dy + y < canvas.height() {
                            canvas.put_pixel(
                                dx + x,
                                dy + y,
                                image::Rgba([px[0], px[1], px[2], px[3]]),
                            );
                        }
                    }
                }
            }
            Some(())
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// NPC 形象图库: 每格一个 NPC 库 (Data/NPC/{n:02}.Lib 的站立首帧)
///
/// 与 `/api/icons` 不同 — 那里是同一个库里的连续帧, 这里是逐个库取首帧。
pub(crate) async fn api_npc_grid(
    st: State<AppState>,
    headers: HeaderMap,
    q: Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    api_sprite_grid(st, headers, AxPath("npc".to_string()), q).await
}

/// 精灵形象网格: 每格一个库的站立首帧 (kind = npc / monster)
pub(crate) async fn api_sprite_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(kind): AxPath<String>,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !matches!(kind.as_str(), "npc" | "monster" | "weapon" | "armour") {
        return Err(StatusCode::NOT_FOUND);
    }
    let count = q.count.min(64);
    let (cols, cw, ch) = (8usize, 96u32, 120u32);
    let rows = count.div_ceil(cols).max(1);
    let start = q.start;
    // 武器/衣甲按 packs 布局取朝南站立帧 32 (站 0+dir*8, dir4=南);
    // npc/monster 库首帧常是占位小图, 取不到像样的就向后扫第一个实帧
    let frame_idx = if matches!(kind.as_str(), "weapon" | "armour") {
        32
    } else {
        0
    };
    let png = tokio::task::spawn_blocking(move || {
        let mut canvas = image::RgbaImage::new(cols as u32 * cw, rows as u32 * ch);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        for i in 0..count {
            let n = (start + i) as u16;
            let (ox, oy) = (((i % cols) as u32) * cw, ((i / cols) as u32) * ch);
            with_preview_lib(&kind, n, |lib| {
                let img = lib
                    .image(frame_idx)
                    .ok()
                    .flatten()
                    .filter(|f| f.width >= 12 && f.height >= 12)
                    .or_else(|| {
                        (0..lib.len().min(900)).find_map(|i| {
                            lib.image(i)
                                .ok()
                                .flatten()
                                .filter(|f| f.width >= 12 && f.height >= 12)
                        })
                    })?;
                let (w, h) = (img.width as u32, img.height as u32);
                // 水平居中, 垂直贴底 (NPC 立绘基准在脚下)
                let dx = ox + cw.saturating_sub(w) / 2;
                let dy = oy + ch.saturating_sub(h.min(ch));
                for y in 0..h.min(ch) {
                    for x in 0..w.min(cw) {
                        let si = ((y * w + x) * 4) as usize;
                        let px = &img.rgba[si..si + 4];
                        if px[3] > 0 && dx + x < canvas.width() && dy + y < canvas.height() {
                            canvas.put_pixel(
                                dx + x,
                                dy + y,
                                image::Rgba([px[0], px[1], px[2], px[3]]),
                            );
                        }
                    }
                }
                Some(())
            });
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 单帧 PNG: items 图标 / weapon-armour 站立帧 (帧 16 = 朝南)
#[derive(Deserialize)]
pub(crate) struct FrameQuery {
    /// 怪物库内基址 (一库多怪时从该帧起找代表帧)
    #[serde(default)]
    base: u32,
}

pub(crate) async fn api_frame_png(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((kind, n)): AxPath<(String, u16)>,
    Query(fq): Query<FrameQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let png = tokio::task::spawn_blocking(move || {
        let (lib_n, frame) = match kind.as_str() {
            "items" => (0u16, n as usize),
            // 外观预览: 该库朝南站立首帧 (packs 布局 0+dir*8, dir4=南)
            "weapon" | "armour" => (n, 32usize),
            // 怪物: 取不到就靠下面的扫描回退
            "monster" => (n, 32usize),
            // 小地图: 库内第 n 帧
            "minimap" => (0u16, n as usize),
            // NPC: 站立首帧
            "npc" => (n, 0usize),
            _ => return None,
        };
        with_preview_lib(&kind, lib_n, |lib| {
            // 首选帧取不到像样的 (市售包大量 1×1 占位) 就向后扫第一个实帧;
            // items/minimap 帧号即语义, 不做扫描回退
            let scan_ok = matches!(kind.as_str(), "weapon" | "armour" | "monster" | "npc");
            let start = fq.base as usize;
            let img = lib
                .image(start + frame)
                .ok()
                .flatten()
                .filter(|f| !scan_ok || (f.width >= 12 && f.height >= 12))
                .or_else(|| {
                    scan_ok
                        .then(|| {
                            (start..(start + 900).min(lib.len())).find_map(|i| {
                                lib.image(i)
                                    .ok()
                                    .flatten()
                                    .filter(|f| f.width >= 12 && f.height >= 12)
                            })
                        })
                        .flatten()
                })?;
            let buf =
                image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba.clone())?;
            let mut out = std::io::Cursor::new(Vec::new());
            buf.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Some(out.into_inner())
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 怪物库内实体候选段: 首实帧 + 每个"长空洞"(≥2 个方向块) 之后的块起始。
/// 一库多怪的素材靠它做二级外观选择
pub(crate) async fn api_mon_bases(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(n): AxPath<u16>,
) -> Result<Json<Vec<u32>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        with_preview_lib("monster", n, |lib| {
            let real = |i: usize| lib.dims(i).is_some_and(|(w, h)| w >= 8 && h >= 8);
            let cap = lib.len().min(6000);
            let base0 = (0..cap).find(|&i| real(i))?;
            // 跨度: 首块连续实帧后的下一个实帧间隔
            let run = (1..64).find(|&d| !real(base0 + d)).unwrap_or(64);
            let stride = (run..64)
                .find(|&d| real(base0 + d))
                .unwrap_or(10)
                .clamp(run, 32);
            let mut out = vec![base0 as u32];
            let mut i = base0;
            let mut gap = 0usize;
            while i < cap {
                if real(i) {
                    if gap >= stride {
                        out.push(i as u32);
                    }
                    gap = 0;
                } else {
                    gap += 1;
                }
                i += 1;
            }
            out.truncate(64);
            Some(out)
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(out))
}

/// 游戏更新静态目录 (登录器下载 manifest.json 与文件)。
/// MIRFORGE_UPDATES 可配, 默认工作目录 updates/; 路径白名单防穿越
pub(crate) async fn api_update_file(
    AxPath(path): AxPath<String>,
) -> Result<axum::response::Response, StatusCode> {
    if path.contains("..") || path.contains('\\') || path.starts_with('/') {
        return Err(StatusCode::FORBIDDEN);
    }
    let root = std::env::var("MIRFORGE_UPDATES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("updates"));
    let full = root.join(&path);
    let bytes = tokio::fs::read(&full)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let ctype = if path.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    };
    Ok(axum::response::Response::builder()
        .header("content-type", ctype)
        .header("cache-control", "no-cache")
        .body(axum::body::Body::from(bytes))
        .unwrap())
}

/// 任意 packs 库的段候选 (帧段空洞切分) — 技能特效选段等通用
/// 返回 [起始帧, 段内实帧估数] 列表
pub(crate) async fn api_packs_bases(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<Json<Vec<(u32, u32)>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let real = |i: usize| lib.dims(i).is_some_and(|(w, h)| w >= 8 && h >= 8);
        let cap = lib.len().min(6000);
        let base0 = (0..cap).find(|&i| real(i))?;
        let mut segs: Vec<(u32, u32)> = Vec::new();
        let (mut start, mut count, mut gap) = (base0, 0u32, 0usize);
        let mut i = base0;
        while i < cap {
            if real(i) {
                if gap >= 8 {
                    segs.push((start as u32, count));
                    start = i;
                    count = 0;
                }
                gap = 0;
                count += 1;
            } else {
                gap += 1;
            }
            i += 1;
        }
        if count > 0 {
            segs.push((start as u32, count));
        }
        segs.truncate(64);
        Some(segs)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(out))
}

// ── 资源查看器: 浏览 packs/ 下任意 .mfl 的帧 ──

/// 查看器库缓存 (最多同时持有几个 — 地图大库上百 MB, 不能无限攒)
static VIEW_LIBS: std::sync::Mutex<
    Option<std::collections::HashMap<String, std::sync::Arc<mir_formats::mfl::AnyLib>>>,
> = std::sync::Mutex::new(None);

/// 相对路径白名单校验: 只允许 packs 根下的 .mfl, 杜绝任意文件读取
fn viewer_path(rel: &str) -> Option<std::path::PathBuf> {
    if rel.contains("..") || rel.starts_with('/') || rel.contains('\\') || !rel.ends_with(".mfl") {
        return None;
    }
    Some(packs_root().join(rel))
}

fn viewer_lib(rel: &str) -> Option<std::sync::Arc<mir_formats::mfl::AnyLib>> {
    let mut guard = VIEW_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(Default::default);
    if let Some(l) = cache.get(rel) {
        return Some(l.clone());
    }
    let lib = mir_formats::mfl::AnyLib::open(&viewer_path(rel)?).ok()?;
    if cache.len() >= 4 {
        cache.clear(); // 简单上限: 查看器串行使用, 清空即可
    }
    let arc = std::sync::Arc::new(lib);
    cache.insert(rel.to_string(), arc.clone());
    Some(arc)
}

#[derive(Serialize)]
pub(crate) struct PackEntry {
    path: String,
    size: u64,
}

/// packs/ 下全部 .mfl 清单 (相对路径 + 字节数)
pub(crate) async fn api_packs_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PackEntry>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<PackEntry>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("mfl") {
                if let (Ok(rel), Ok(meta)) = (p.strip_prefix(base), e.metadata()) {
                    out.push(PackEntry {
                        path: rel.to_string_lossy().replace('\\', "/"),
                        size: meta.len(),
                    });
                }
            }
        }
    }
    let root = packs_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(out))
}

#[derive(Deserialize)]
pub(crate) struct ViewerQuery {
    file: String,
    #[serde(default)]
    idx: usize,
}

#[derive(Serialize)]
pub(crate) struct PackInfo {
    frames: usize,
    real: usize,
}

/// 单库信息: 帧位数与实帧数
pub(crate) async fn api_packs_info(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<Json<PackInfo>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let info = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let mut real = 0;
        for i in 0..lib.len() {
            if lib.image(i).ok().flatten().is_some() {
                real += 1;
            }
        }
        Some(PackInfo {
            frames: lib.len(),
            real,
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(info))
}

/// 单帧 PNG (空帧/越界 404); 响应头带尺寸与锚点供前端展示
pub(crate) async fn api_packs_frame(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let img = lib.image(q.idx).ok().flatten()?;
        let buf =
            image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba.clone())?;
        let mut png = std::io::Cursor::new(Vec::new());
        buf.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some((
            png.into_inner(),
            img.width,
            img.height,
            img.offset_x,
            img.offset_y,
        ))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let (png, w, h, ox, oy) = out;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .header("x-frame-meta", format!("{w}x{h} 锚({ox},{oy})"))
        .body(axum::body::Body::from(png))
        .unwrap())
}

#[derive(Deserialize)]
pub(crate) struct StripQuery {
    file: String,
    #[serde(default)]
    base: usize,
    #[serde(default)]
    frames: usize,
    /// 扫描窗硬上限 (0 = 默认 frames*4+32); 起手/飞行等 10 槽块预览用,
    /// 防止越过块边界吸入邻段帧
    #[serde(default)]
    span: usize,
}

/// 成品拼条缓存 (段级; 特效库解帧+缩放不便宜, 弹层反复开)
type StripCache = std::collections::HashMap<(String, usize, usize), std::sync::Arc<Vec<u8>>>;
static STRIP_CACHE: std::sync::Mutex<Option<StripCache>> = std::sync::Mutex::new(None);

/// 特效段动画拼条: 自 base 起收集至多 frames 个实帧, 按各帧锚点对齐到
/// 公共包围盒后缩放进 96px 格子, 横拼一条 PNG。前端用 CSS steps() 循环
/// 播放, 一段一个请求就能看完整动画。
pub(crate) async fn api_packs_strip(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StripQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    const CELL: u32 = 96;
    let n = q.frames.clamp(1, 30);
    let key = (q.file.clone(), q.base, n + q.span * 100);
    if let Ok(mut g) = STRIP_CACHE.lock() {
        if let Some(hit) = g.get_or_insert_with(Default::default).get(&key) {
            return Ok(strip_response(hit.as_ref().clone()));
        }
    }
    let png = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        // 收集实帧 (容忍段内空洞, 扫描窗口有界)
        let mut imgs = Vec::new();
        let win = if q.span > 0 { q.span } else { n * 4 + 32 };
        let end = (q.base + win).min(lib.len());
        for i in q.base..end {
            if imgs.len() >= n {
                break;
            }
            if let Ok(Some(img)) = lib.image(i) {
                if img.width >= 2 && img.height >= 2 {
                    imgs.push(img);
                }
            }
        }
        if imgs.is_empty() {
            return None;
        }
        // 锚点公共包围盒: 每帧真实相对位置对齐, 动画不抖
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for im in &imgs {
            x0 = x0.min(im.offset_x as i32);
            y0 = y0.min(im.offset_y as i32);
            x1 = x1.max(im.offset_x as i32 + im.width as i32);
            y1 = y1.max(im.offset_y as i32 + im.height as i32);
        }
        let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
        let scale = (CELL as f32 / bw).min(CELL as f32 / bh).min(1.0);
        let (pad_x, pad_y) = (
            (CELL as f32 - bw * scale) / 2.0,
            (CELL as f32 - bh * scale) / 2.0,
        );
        let mut canvas = image::RgbaImage::new(n as u32 * CELL, CELL);
        for (i, im) in imgs.iter().enumerate() {
            let cell_x = i as u32 * CELL;
            let dw = (im.width as f32 * scale).ceil() as u32;
            let dh = (im.height as f32 * scale).ceil() as u32;
            let ox = pad_x + (im.offset_x as i32 - x0) as f32 * scale;
            let oy = pad_y + (im.offset_y as i32 - y0) as f32 * scale;
            for dy in 0..dh {
                let sy = (dy as f32 / scale) as usize;
                let ty = oy as u32 + dy;
                if sy >= im.height as usize || ty >= CELL {
                    continue;
                }
                for dx in 0..dw {
                    let sx = (dx as f32 / scale) as usize;
                    let tx = cell_x + ox as u32 + dx;
                    if sx >= im.width as usize || tx >= (i as u32 + 1) * CELL {
                        continue;
                    }
                    let si = (sy * im.width as usize + sx) * 4;
                    let px = &im.rgba[si..si + 4];
                    if px[3] > 0 {
                        canvas.put_pixel(tx, ty, image::Rgba([px[0], px[1], px[2], px[3]]));
                    }
                }
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        canvas.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(png.into_inner())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let arc = std::sync::Arc::new(png.clone());
    if let Ok(mut g) = STRIP_CACHE.lock() {
        let c = g.get_or_insert_with(Default::default);
        if c.len() > 128 {
            c.clear();
        }
        c.insert(key, arc);
    }
    Ok(strip_response(png))
}

fn strip_response(png: Vec<u8>) -> axum::response::Response {
    axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap()
}

/// 小地图选择网格: mmap.Lib 帧缩放到单元格 (5 列)
pub(crate) async fn api_minimap_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let count = q.count.clamp(1, 60);
    let png = tokio::task::spawn_blocking(move || {
        let (cols, cw, ch) = (5u32, 128u32, 88u32);
        let rows = (count as u32).div_ceil(cols).max(1);
        let mut canvas = image::RgbaImage::new(cols * cw, rows * ch);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        with_preview_lib("minimap", 0, |lib| {
            for i in 0..count {
                let Ok(Some(src)) = lib.image(q.start + i) else {
                    continue;
                };
                let (sw, sh) = (src.width as u32, src.height as u32);
                if sw == 0 || sh == 0 {
                    continue;
                }
                let (ox, oy) = ((i as u32 % cols) * cw, (i as u32 / cols) * ch);
                // 等比缩放进单元格 (留 4px 边)
                let sc = ((cw - 6) as f32 / sw as f32).min((ch - 6) as f32 / sh as f32);
                let (dw, dh) = (
                    ((sw as f32 * sc) as u32).max(1),
                    ((sh as f32 * sc) as u32).max(1),
                );
                for dy in 0..dh {
                    for dx in 0..dw {
                        let (sx2, sy2) = ((dx as f32 / sc) as u32, (dy as f32 / sc) as u32);
                        if sx2 >= sw || sy2 >= sh {
                            continue;
                        }
                        let si = ((sy2 * sw + sx2) * 4) as usize;
                        let px = &src.rgba[si..si + 4];
                        if px[3] > 0 {
                            canvas.put_pixel(
                                ox + 3 + dx,
                                oy + 3 + dy,
                                image::Rgba([px[0], px[1], px[2], 255]),
                            );
                        }
                    }
                }
            }
            Some(())
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 审计日志: 管理台写操作追加一行 JSON 到 admin-audit.log
pub(crate) fn audit(action: &str, detail: &str) {
    let line = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "action": action,
        "detail": detail,
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("admin-audit.log")
    {
        let _ = writeln!(f, "{line}");
    }
    tracing::info!("管理操作: {action} {detail}");
}
