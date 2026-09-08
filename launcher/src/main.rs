//! MirForge 登录器。
//!
//! 职责: 账号 (登录/注册/密保找回) → 一次性启动票据 → 拉起客户端;
//! 服务器选择 (servers.json)、公告栏 (服务端公开端点)、
//! 窗口设置 (经环境变量传给客户端)、游戏更新 (manifest 按文件增量)。
//!
//! 网络全部在后台线程跑 (WS 短连接 + 阻塞 HTTP), 经 mpsc 回报 UI;
//! egui 界面永不阻塞。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;
use protocol::{ClientMessage, ServerMessage, PROTOCOL_VERSION};

// ─────────── 配置 ───────────

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ServerEntry {
    name: String,
    /// 游戏服 ws 地址
    game: String,
    /// 公告/更新 http 地址 (管理台端口)
    http: String,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Settings {
    last_server: usize,
    username: String,
    window: String,
    fullscreen: bool,
    /// 客户端 exe 路径 (空 = 自动探测: 同目录 → 开发布局)
    #[serde(default)]
    client_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            last_server: 0,
            username: String::new(),
            window: "1600x900".into(),
            fullscreen: false,
            client_path: String::new(),
        }
    }
}

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_servers() -> Vec<ServerEntry> {
    let p = base_dir().join("servers.json");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str(&s) {
            return v;
        }
    }
    let def = vec![ServerEntry {
        name: "本机测试服".into(),
        game: "ws://127.0.0.1:4000".into(),
        http: "http://127.0.0.1:4001".into(),
    }];
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&def).unwrap());
    def
}

fn load_settings() -> Settings {
    std::fs::read_to_string(base_dir().join("launcher.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    let _ = std::fs::write(
        base_dir().join("launcher.json"),
        serde_json::to_string_pretty(s).unwrap(),
    );
}

// ─────────── 后台任务回报 ───────────

enum Report {
    /// 登录/注册/找回的结果 (成功?, 提示语, 票据)
    Auth(bool, String, Option<String>),
    News(Vec<NewsItem>),
    /// 更新检查: 需下载的文件数与总字节 (0 = 已最新); None = 检查失败
    UpdatePlan(Option<(usize, u64)>),
    /// 下载进度 (已完成文件数, 总文件数, 当前文件名)
    UpdateProgress(usize, usize, String),
    UpdateDone(Result<(), String>),
}

#[derive(serde::Deserialize, Clone)]
struct NewsItem {
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    pinned: bool,
    #[serde(default)]
    created_at: String,
}

// ─────────── WS 短连接会话 (登录/注册/找回 → 票据) ───────────

/// 连接 → hello → 发请求 → 收关键应答 → 断开。
/// 返回 (成功?, 提示, 票据)
fn ws_auth(game_url: &str, action: AuthAction) -> (bool, String, Option<String>) {
    let (mut ws, _) = match tungstenite::connect(game_url) {
        Ok(v) => v,
        Err(e) => return (false, format!("连不上服务器: {e}"), None),
    };
    let send = |ws: &mut tungstenite::WebSocket<_>, m: &ClientMessage| {
        let _ = ws.send(tungstenite::Message::Text(
            serde_json::to_string(m).unwrap(),
        ));
    };
    send(
        &mut ws,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    );
    let mut logged_in = false;
    let mut msg_text = String::new();
    // 简单收发状态机: 最多轮询 ~10 秒
    for _ in 0..200 {
        let Ok(m) = ws.read() else { break };
        let tungstenite::Message::Text(t) = m else {
            continue;
        };
        let Ok(sm) = serde_json::from_str::<ServerMessage>(&t) else {
            continue;
        };
        match sm {
            ServerMessage::HelloAck { .. } => match &action {
                AuthAction::Login { user, pass } => send(
                    &mut ws,
                    &ClientMessage::Login {
                        username: user.clone(),
                        password: pass.clone(),
                    },
                ),
                AuthAction::Register {
                    user,
                    pass,
                    question,
                    answer,
                } => send(
                    &mut ws,
                    &ClientMessage::Register {
                        username: user.clone(),
                        password: pass.clone(),
                        security_question: (!question.is_empty()).then(|| question.clone()),
                        security_answer: (!answer.is_empty()).then(|| answer.clone()),
                    },
                ),
                AuthAction::Reset {
                    user,
                    answer,
                    new_pass,
                } => send(
                    &mut ws,
                    &ClientMessage::ResetPassword {
                        username: user.clone(),
                        security_answer: answer.clone(),
                        new_password: new_pass.clone(),
                    },
                ),
            },
            ServerMessage::LoginResult {
                success, message, ..
            } => {
                msg_text = message;
                if !success {
                    return (false, msg_text, None);
                }
                match action {
                    // 找回密码: 成功即结束, 不要票据
                    AuthAction::Reset { .. } => return (true, msg_text, None),
                    _ => {
                        logged_in = true;
                        send(&mut ws, &ClientMessage::RequestTicket);
                    }
                }
            }
            ServerMessage::LaunchTicket { ticket } if logged_in => {
                return (true, msg_text, Some(ticket));
            }
            ServerMessage::Error { message } => return (false, message, None),
            _ => {}
        }
    }
    (false, "服务器无响应".into(), None)
}

enum AuthAction {
    Login {
        user: String,
        pass: String,
    },
    Register {
        user: String,
        pass: String,
        question: String,
        answer: String,
    },
    Reset {
        user: String,
        answer: String,
        new_pass: String,
    },
}

// ─────────── 更新 ───────────

#[derive(serde::Deserialize)]
struct Manifest {
    version: String,
    files: Vec<ManifestFile>,
}

#[derive(serde::Deserialize, Clone)]
struct ManifestFile {
    path: String,
    #[allow(dead_code)]
    size: u64,
    sha256: String,
}

fn sha256_file(p: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(p).ok()?;
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// 拉清单比对本地, 返回 (版本, 差异文件)。
fn plan_update(http: &str) -> Result<(String, Vec<ManifestFile>), String> {
    let url = format!("{http}/updates/manifest.json");
    let m: Manifest = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .map_err(|e| format!("拉取清单失败: {e}"))?
        .into_json()
        .map_err(|e| format!("清单解析失败: {e}"))?;
    let base = base_dir();
    // 本地已装清单缓存: 路径 → hash (避免每次全量哈希)
    let cache_path = base.join("installed.json");
    let cache: std::collections::HashMap<String, String> = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut need = Vec::new();
    for f in &m.files {
        let local = base.join(&f.path);
        let local_hash = match cache.get(&f.path) {
            Some(h) if local.exists() => Some(h.clone()),
            _ => sha256_file(&local),
        };
        if local_hash.as_deref() != Some(f.sha256.as_str()) {
            need.push(f.clone());
        }
    }
    Ok((m.version, need))
}

fn run_update(http: String, need: Vec<ManifestFile>, tx: Sender<Report>) {
    let base = base_dir();
    let total = need.len();
    let mut installed: std::collections::HashMap<String, String> =
        std::fs::read_to_string(base.join("installed.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
    for (i, f) in need.iter().enumerate() {
        let _ = tx.send(Report::UpdateProgress(i, total, f.path.clone()));
        let url = format!("{http}/updates/{}", f.path);
        let res = (|| -> Result<(), String> {
            let resp = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(300))
                .call()
                .map_err(|e| format!("{}: {e}", f.path))?;
            let mut bytes = Vec::new();
            use std::io::Read;
            resp.into_reader()
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{}: {e}", f.path))?;
            use sha2::{Digest, Sha256};
            let got = format!("{:x}", Sha256::digest(&bytes));
            if got != f.sha256 {
                return Err(format!("{}: 校验失败", f.path));
            }
            let dst = base.join(&f.path);
            if let Some(dir) = dst.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // 临时文件 + 原子替换
            let tmp = dst.with_extension("mfdl");
            std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
            Ok(())
        })();
        if let Err(e) = res {
            let _ = tx.send(Report::UpdateDone(Err(e)));
            return;
        }
        installed.insert(f.path.clone(), f.sha256.clone());
        let _ = std::fs::write(
            base.join("installed.json"),
            serde_json::to_string(&installed).unwrap(),
        );
    }
    let _ = tx.send(Report::UpdateProgress(total, total, String::new()));
    let _ = tx.send(Report::UpdateDone(Ok(())));
}

// ─────────── UI ───────────

// ─────────── 主题 (与游戏 HUD 同风格: 暗底描金) ───────────

const BG: egui::Color32 = egui::Color32::from_rgb(20, 17, 12);
const PANEL: egui::Color32 = egui::Color32::from_rgb(31, 26, 18);
const PANEL_2: egui::Color32 = egui::Color32::from_rgb(38, 32, 22);
const EDGE: egui::Color32 = egui::Color32::from_rgb(66, 55, 36);
const GOLD: egui::Color32 = egui::Color32::from_rgb(208, 163, 82);
const GOLD_DIM: egui::Color32 = egui::Color32::from_rgb(140, 112, 60);
const INK: egui::Color32 = egui::Color32::from_rgb(232, 224, 208);
const INK_WEAK: egui::Color32 = egui::Color32::from_rgb(150, 138, 118);
const DANGER: egui::Color32 = egui::Color32::from_rgb(220, 120, 100);

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(INK);
    v.panel_fill = egui::Color32::TRANSPARENT;
    v.window_fill = PANEL;
    v.extreme_bg_color = egui::Color32::from_rgb(14, 12, 8); // 输入框底
    v.faint_bg_color = PANEL_2;
    v.widgets.inactive.bg_fill = PANEL_2;
    v.widgets.inactive.weak_bg_fill = PANEL_2;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, EDGE);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, INK);
    v.widgets.hovered.bg_fill = egui::Color32::from_rgb(52, 43, 28);
    v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(52, 43, 28);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, GOLD_DIM);
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.2, INK);
    v.widgets.active.bg_fill = egui::Color32::from_rgb(64, 52, 32);
    v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(64, 52, 32);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.2, GOLD);
    v.widgets.open.bg_fill = PANEL_2;
    v.widgets.open.weak_bg_fill = PANEL_2;
    v.selection.bg_fill = egui::Color32::from_rgb(90, 70, 36);
    v.selection.stroke = egui::Stroke::new(1.0, GOLD);
    v.hyperlink_color = GOLD;
    ctx.set_visuals(v);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    use egui::{FontId, TextStyle};
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(21.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.5));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.5));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    ctx.set_style(style);
}

/// 卡片容器 (面板底 + 细金边 + 圆角)
fn card() -> egui::Frame {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, EDGE))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .shadow(egui::Shadow {
            offset: [0, 3],
            blur: 14,
            spread: 0,
            color: egui::Color32::from_black_alpha(110),
        })
}

/// 垂直渐变矩形 (Mesh 顶点色)
fn vgrad(painter: &egui::Painter, rect: egui::Rect, top: egui::Color32, bottom: egui::Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(egui::Shape::mesh(mesh));
}

/// 颜色线性插值 (Color32 内部为预乘, 直接插分量)
/// 窗口圆角半径: 底色/渐变/光晕/描边必须共用, 不同心就会在四角露出色环
const WIN_RADIUS: f32 = 14.0;

fn lerp_c(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgba_premultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

/// 带圆角帽的垂直渐变: 上下各留一段圆角矩形, 中间走 mesh 渐变。
/// 直接用方角 mesh 会盖住窗口圆角 (四角露出直角色块) —— 这是它存在的原因。
fn vgrad_rounded(
    painter: &egui::Painter,
    rect: egui::Rect,
    top: egui::Color32,
    bottom: egui::Color32,
    r_top: f32,
    r_bot: f32,
) {
    let h = rect.height().max(1.0);
    let (r_top, r_bot) = (r_top.min(h * 0.25), r_bot.min(h * 0.25));
    // 圆角帽高度必须 >= 2×半径: egui 会把圆角半径钳到矩形短边的一半,
    // 帽子太矮圆角就被削掉一半, 填充的弧比描边的弧小一圈 (四角看着像
    // 内圈多了一条棕线)。帽内必须用「与中段接缝处」的颜色填充 (顶帽取
    // 帽底色, 底帽取帽顶色), 接缝才无阶跃; 若取帽中点色, 快速渐变
    // (如顶部氛围光) 会在帽边缘留下一条清晰的横向色带分界线。
    let (cap_t, cap_b) = (r_top * 2.0, r_bot * 2.0);
    if r_top > 0.5 {
        painter.rect_filled(
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + cap_t)),
            egui::CornerRadius {
                nw: r_top as u8,
                ne: r_top as u8,
                sw: 0,
                se: 0,
            },
            lerp_c(top, bottom, cap_t / h),
        );
    }
    if r_bot > 0.5 {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - cap_b), rect.max),
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: r_bot as u8,
                se: r_bot as u8,
            },
            lerp_c(top, bottom, (h - cap_b) / h),
        );
    }
    let mid = egui::Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y + cap_t),
        egui::pos2(rect.max.x, rect.max.y - cap_b),
    );
    if mid.height() > 0.5 {
        vgrad(
            painter,
            mid,
            lerp_c(top, bottom, cap_t / h),
            lerp_c(top, bottom, (h - cap_b) / h),
        );
    }
}

/// 渐变金主按钮 (hover 提亮 / 按下微沉 / 禁用暗金)
fn gold_button(ui: &mut egui::Ui, label: &str, size: egui::Vec2, enabled: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = enabled && resp.hovered();
    let pressed = enabled && resp.is_pointer_button_down_on();
    let (top, bot, edge) = if !enabled {
        (
            egui::Color32::from_rgb(110, 90, 52),
            egui::Color32::from_rgb(88, 71, 40),
            egui::Color32::from_rgb(120, 98, 58),
        )
    } else if pressed {
        (
            egui::Color32::from_rgb(178, 138, 66),
            egui::Color32::from_rgb(150, 115, 54),
            GOLD,
        )
    } else if hovered {
        (
            egui::Color32::from_rgb(238, 196, 116),
            egui::Color32::from_rgb(204, 158, 76),
            egui::Color32::from_rgb(248, 216, 150),
        )
    } else {
        (
            egui::Color32::from_rgb(222, 178, 98),
            egui::Color32::from_rgb(186, 142, 64),
            egui::Color32::from_rgb(236, 200, 128),
        )
    };
    let p = ui.painter();
    p.rect_filled(rect, 8.0, bot);
    // 上半高光渐变 (内缩避开圆角)
    let top_half = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, rect.top() + 1.5),
        egui::pos2(rect.right() - 4.0, rect.center().y),
    );
    vgrad(p, top_half, top, bot.gamma_multiply(1.0));
    p.rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, edge),
        egui::StrokeKind::Inside,
    );
    let text_c = if enabled {
        BG
    } else {
        egui::Color32::from_rgb(40, 33, 22)
    };
    p.text(
        rect.center() + egui::vec2(0.0, if pressed { 1.0 } else { 0.0 }),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(16.0),
        text_c,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// 分段式页签 (整条圆角槽 + 选中金块)
fn segmented(ui: &mut egui::Ui, width: f32, items: &[&str], sel: &mut usize) -> bool {
    let h = 32.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, h), egui::Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 8.0, egui::Color32::from_rgb(15, 12, 8));
    p.rect_stroke(
        rect,
        8.0,
        egui::Stroke::new(1.0, EDGE),
        egui::StrokeKind::Inside,
    );
    let seg_w = width / items.len() as f32;
    let mut changed = false;
    if let (true, Some(pos)) = (resp.clicked(), resp.interact_pointer_pos()) {
        let idx = (((pos.x - rect.left()) / seg_w) as usize).min(items.len() - 1);
        if idx != *sel {
            *sel = idx;
            changed = true;
        }
    }
    for (i, label) in items.iter().enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * seg_w, rect.top()),
            egui::vec2(seg_w, h),
        );
        if i == *sel {
            let inner = seg.shrink(3.0);
            p.rect_filled(inner, 6.0, egui::Color32::from_rgb(88, 68, 34));
            p.rect_stroke(
                inner,
                6.0,
                egui::Stroke::new(1.0, GOLD_DIM),
                egui::StrokeKind::Inside,
            );
        }
        p.text(
            seg.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.5),
            if i == *sel { GOLD } else { INK_WEAK },
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    changed
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Login,
    Register,
    Reset,
}

struct App {
    servers: Vec<ServerEntry>,
    settings: Settings,
    tab: Tab,
    user: String,
    pass: String,
    pass2: String,
    question: String,
    answer: String,
    status: String,
    news: Vec<NewsItem>,
    busy: bool,
    /// 已登录账号 (None = 未登录; 登录后才可选区服/进入游戏)
    logged: Option<String>,
    /// 设置弹层 (齿轮按钮)
    show_settings: bool,
    /// 区服选择弹层 (账号卡区服行点击)
    show_server_pick: bool,
    /// 本次认证成功后是否直接启动游戏 ("进入游戏" 按钮置位)
    pending_launch: bool,
    ticket: Option<String>,
    update_state: UpdateState,
    tx: Sender<Report>,
    rx: Receiver<Report>,
}

enum UpdateState {
    Checking,
    UpToDate,
    Available(usize, u64),
    Downloading(usize, usize, String),
    Failed(String),
}

impl App {
    fn new(ctx: &eframe::CreationContext<'_>) -> Self {
        // 中文字体 (egui 默认无 CJK)
        for cand in [
            "C:/Windows/Fonts/msyh.ttc",
            "C:/Windows/Fonts/simhei.ttf",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ] {
            if let Ok(bytes) = std::fs::read(cand) {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.entry(family).or_default().push("cjk".into());
                }
                ctx.egui_ctx.set_fonts(fonts);
                break;
            }
        }
        apply_theme(&ctx.egui_ctx);
        let servers = load_servers();
        let settings = load_settings();
        let (tx, rx) = channel();
        let mut app = Self {
            user: settings.username.clone(),
            servers,
            settings,
            tab: Tab::Login,
            pass: String::new(),
            pass2: String::new(),
            question: String::new(),
            answer: String::new(),
            status: String::new(),
            news: Vec::new(),
            busy: false,
            logged: None,
            show_settings: false,
            show_server_pick: false,
            pending_launch: false,
            ticket: None,
            update_state: UpdateState::Checking,
            tx,
            rx,
        };
        app.settings.last_server = app
            .settings
            .last_server
            .min(app.servers.len().saturating_sub(1));
        app.refresh_remote();
        app
    }

    fn server(&self) -> &ServerEntry {
        &self.servers[self.settings.last_server]
    }

    /// 拉公告 + 检查更新 (后台)
    fn refresh_remote(&mut self) {
        self.update_state = UpdateState::Checking;
        let http = self.server().http.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Ok(resp) = ureq::get(&format!("{http}/api/public/news"))
                .timeout(std::time::Duration::from_secs(5))
                .call()
            {
                if let Ok(items) = resp.into_json::<Vec<NewsItem>>() {
                    let _ = tx.send(Report::News(items));
                }
            }
            match plan_update(&http) {
                Ok((_, need)) => {
                    let bytes = need.iter().map(|f| f.size).sum();
                    let _ = tx.send(Report::UpdatePlan(Some((need.len(), bytes))));
                }
                Err(_) => {
                    let _ = tx.send(Report::UpdatePlan(None));
                }
            }
        });
    }

    fn auth(&mut self, action: AuthAction) {
        self.busy = true;
        self.status = "处理中...".into();
        let game = self.server().game.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (ok, msg, ticket) = ws_auth(&game, action);
            let _ = tx.send(Report::Auth(ok, msg, ticket));
        });
    }

    fn start_update(&mut self) {
        let http = self.server().http.clone();
        let tx = self.tx.clone();
        self.update_state = UpdateState::Downloading(0, 0, "准备...".into());
        std::thread::spawn(move || match plan_update(&http) {
            Ok((_, need)) => run_update(http, need, tx),
            Err(e) => {
                let _ = tx.send(Report::UpdateDone(Err(e)));
            }
        });
    }

    /// 探测客户端 exe: 配置优先 → 同目录 → 开发布局
    fn client_exe(&self) -> Option<PathBuf> {
        if !self.settings.client_path.is_empty() {
            let p = PathBuf::from(&self.settings.client_path);
            if p.exists() {
                return Some(p);
            }
        }
        let cands = [
            base_dir().join("mirforge-client.exe"),
            base_dir().join("target/debug/mirforge-client.exe"),
            base_dir().join("../../../target/debug/mirforge-client.exe"),
        ];
        cands.into_iter().find(|p| p.exists())
    }

    fn launch_game(&mut self, ticket: &str) {
        let Some(exe) = self.client_exe() else {
            self.status = "找不到客户端程序 (mirforge-client.exe)".into();
            return;
        };
        let mut cmd = std::process::Command::new(&exe);
        cmd.env("MIRFORGE_SERVER", &self.server().game)
            .env("MIRFORGE_TICKET", ticket)
            .env("MIRFORGE_WINDOW", &self.settings.window);
        if self.settings.fullscreen {
            cmd.env("MIRFORGE_FULLSCREEN", "1");
        }
        if let Some(dir) = exe.parent() {
            // 开发布局下工作目录用仓库根 (packs 相对定位)
            let root = dir.join("../..");
            cmd.current_dir(if root.join("packs").exists() {
                root
            } else {
                dir.to_path_buf()
            });
        }
        match cmd.spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => self.status = format!("启动失败: {e}"),
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // 透明底: 圆角窗口四角不留黑块
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 后台回报
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Report::Auth(ok, msg, ticket) => {
                    self.busy = false;
                    self.status = msg;
                    match (ok, ticket) {
                        // 登录/注册成功: 驻留登录器 (显示账号与区服),
                        // 只有点了「进入游戏」才拿票据拉起客户端
                        (true, Some(t)) => {
                            self.logged = Some(self.user.clone());
                            self.settings.username = self.user.clone();
                            save_settings(&self.settings);
                            self.pass2.clear();
                            if self.pending_launch {
                                self.ticket = Some(t.clone());
                                self.launch_game(&t);
                            } else {
                                self.status = format!("欢迎回来, {}", self.user);
                            }
                        }
                        // 找回密码成功: 回登录页
                        (true, None) => self.tab = Tab::Login,
                        _ => {}
                    }
                    self.pending_launch = false;
                }
                Report::News(n) => self.news = n,
                Report::UpdatePlan(Some((0, _))) => self.update_state = UpdateState::UpToDate,
                Report::UpdatePlan(Some((n, bytes))) => {
                    self.update_state = UpdateState::Available(n, bytes)
                }
                Report::UpdatePlan(None) => {
                    self.update_state = UpdateState::Failed("更新源不可达 (可跳过)".into())
                }
                Report::UpdateProgress(done, total, file) => {
                    self.update_state = UpdateState::Downloading(done, total, file)
                }
                Report::UpdateDone(Ok(())) => self.update_state = UpdateState::UpToDate,
                Report::UpdateDone(Err(e)) => self.update_state = UpdateState::Failed(e),
            }
            ctx.request_repaint();
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let painter = ui.painter();
                // 窗体: 圆角深底 + 垂直渐变 + 顶部金色氛围光 + 金边 + 角饰
                painter.rect_filled(rect, WIN_RADIUS, BG);
                vgrad_rounded(
                    painter,
                    rect,
                    egui::Color32::from_rgb(28, 23, 15),
                    egui::Color32::from_rgb(16, 13, 9),
                    WIN_RADIUS,
                    WIN_RADIUS,
                );
                let glow = egui::Rect::from_min_max(
                    rect.left_top(),
                    egui::pos2(rect.right(), rect.top() + 90.0),
                );
                vgrad_rounded(
                    painter,
                    glow,
                    egui::Color32::from_rgba_unmultiplied(208, 163, 82, 26),
                    egui::Color32::TRANSPARENT,
                    WIN_RADIUS,
                    0.0,
                );
                // 描边必须与填充同 rect 同半径: shrink 后半径不变会让拐角
                // 曲率错位, 描边在四角缩进成"内圈第二条圆线"
                painter.rect_stroke(
                    rect,
                    WIN_RADIUS,
                    egui::Stroke::new(1.2, egui::Color32::from_rgb(84, 68, 42)),
                    egui::StrokeKind::Inside,
                );
                let inner = rect.shrink(16.0);
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                self.draw_header(ctx, &mut ui);
                ui.add_space(8.0);
                // 主区: 左公告 / 右账号卡
                let body_h = ui.available_height() - 52.0; // 底部更新条预留
                ui.allocate_ui(egui::vec2(ui.available_width(), body_h), |ui| {
                    ui.horizontal_top(|ui| {
                        let right_w = 268.0;
                        let left_w = ui.available_width() - right_w - 12.0;
                        ui.allocate_ui(egui::vec2(left_w, body_h), |ui| {
                            self.draw_news(ui, body_h);
                        });
                        ui.add_space(6.0);
                        ui.allocate_ui(egui::vec2(right_w, body_h), |ui| {
                            // 外层是水平布局, 先转竖排再做垂直居中
                            ui.vertical(|ui| {
                                // 卡高随页签变化, 取上一帧量得的高度算留白
                                let id = egui::Id::new("auth-card-h");
                                let h: f32 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(body_h);
                                ui.add_space(((body_h - h) / 2.0).max(0.0));
                                let top = ui.cursor().top();
                                self.draw_auth(ui);
                                let measured = ui.cursor().top() - top;
                                ui.ctx().data_mut(|d| d.insert_temp(id, measured));
                            });
                        });
                    });
                });
                ui.add_space(8.0);
                self.draw_update_bar(&mut ui);
            });

        if self.show_settings {
            self.draw_settings_popup(ctx);
        }
        if self.show_server_pick {
            self.draw_server_popup(ctx);
        }
    }
}

impl App {
    /// 自绘标题栏: 左 logo, 右设置/最小化/关闭; 空白区可拖动
    fn draw_header(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        // 拖动层必须先于控件注册: egui 命中测试取最后注册的控件, 拖动层若在
        // 控件之后注册就会盖住它们 (线路下拉曾因此点不开)
        let bar_rect =
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 28.0));
        if ui
            .interact(
                bar_rect,
                egui::Id::new("titlebar-drag"),
                egui::Sense::drag(),
            )
            .drag_started()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            // logo 发光重影
            let (r, _) = ui.allocate_exact_size(egui::vec2(150.0, 26.0), egui::Sense::hover());
            let p = ui.painter();
            let f = egui::FontId::proportional(21.0);
            let base = egui::pos2(r.left(), r.center().y);
            p.text(
                base + egui::vec2(1.0, 1.0),
                egui::Align2::LEFT_CENTER,
                "MirForge",
                f.clone(),
                egui::Color32::from_rgba_unmultiplied(208, 163, 82, 60),
            );
            let w = p
                .text(base, egui::Align2::LEFT_CENTER, "Mir", f.clone(), GOLD)
                .width();
            p.text(
                base + egui::vec2(w, 0.0),
                egui::Align2::LEFT_CENTER,
                "Forge",
                f,
                INK,
            );
            p.text(
                egui::pos2(r.left() + 2.0, r.bottom() + 1.0),
                egui::Align2::LEFT_BOTTOM,
                "L A U N C H E R",
                egui::FontId::proportional(8.0),
                GOLD_DIM,
            );
            // 右侧窗控: 自绘线条图标 (齿轮 / 横线 / 交叉), 圆角矩形 hover 底
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let win_btn = |ui: &mut egui::Ui, kind: u8, danger: bool| {
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(36.0, 28.0), egui::Sense::click());
                    let p = ui.painter();
                    let hovered = resp.hovered();
                    if hovered {
                        let bg = if danger {
                            egui::Color32::from_rgb(158, 56, 42)
                        } else {
                            egui::Color32::from_rgb(56, 46, 30)
                        };
                        p.rect_filled(rect, 6.0, bg);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let ic = if hovered {
                        egui::Color32::from_rgb(244, 236, 220)
                    } else {
                        INK_WEAK
                    };
                    let st = egui::Stroke::new(1.4, ic);
                    let c = rect.center();
                    match kind {
                        // 齿轮: 外圈 + 轴心 + 8 齿
                        0 => {
                            p.circle_stroke(c, 5.2, st);
                            p.circle_stroke(c, 1.8, st);
                            for k in 0..8 {
                                let a = k as f32 * std::f32::consts::TAU / 8.0;
                                let d = egui::vec2(a.cos(), a.sin());
                                p.line_segment([c + d * 5.2, c + d * 7.4], st);
                            }
                        }
                        // 最小化: 横线
                        1 => {
                            p.line_segment(
                                [c + egui::vec2(-5.0, 0.0), c + egui::vec2(5.0, 0.0)],
                                st,
                            );
                        }
                        // 关闭: 交叉线
                        _ => {
                            p.line_segment(
                                [c + egui::vec2(-4.6, -4.6), c + egui::vec2(4.6, 4.6)],
                                st,
                            );
                            p.line_segment(
                                [c + egui::vec2(-4.6, 4.6), c + egui::vec2(4.6, -4.6)],
                                st,
                            );
                        }
                    }
                    resp
                };
                if win_btn(ui, 2, true).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if win_btn(ui, 1, false).clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if win_btn(ui, 0, false).clicked() {
                    self.show_settings = !self.show_settings;
                }
            });
        });
        ui.add_space(5.0);
        // 渐变分隔线 (中亮两端隐)
        let r = ui.max_rect();
        let y = ui.cursor().top();
        let p = ui.painter();
        let mid = (r.left() + r.right()) / 2.0;
        let mut mesh = egui::Mesh::default();
        let ce = egui::Color32::TRANSPARENT;
        let cm = egui::Color32::from_rgba_unmultiplied(208, 163, 82, 140);
        mesh.colored_vertex(egui::pos2(r.left(), y), ce);
        mesh.colored_vertex(egui::pos2(mid, y), cm);
        mesh.colored_vertex(egui::pos2(r.left(), y + 1.0), ce);
        mesh.colored_vertex(egui::pos2(mid, y + 1.0), cm);
        mesh.colored_vertex(egui::pos2(r.right(), y), ce);
        mesh.colored_vertex(egui::pos2(r.right(), y + 1.0), ce);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 3, 2);
        mesh.add_triangle(1, 4, 3);
        mesh.add_triangle(4, 5, 3);
        p.add(egui::Shape::mesh(mesh));
    }

    fn draw_news(&mut self, ui: &mut egui::Ui, h: f32) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(4.0, 16.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, GOLD);
                ui.label(
                    egui::RichText::new("新闻公告")
                        .size(16.0)
                        .strong()
                        .color(INK),
                );
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(h - 34.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.news.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("暂无公告").color(INK_WEAK));
                        });
                    }
                    for n in &self.news {
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width() - 8.0);
                            ui.horizontal(|ui| {
                                if n.pinned {
                                    let (r, _) = ui.allocate_exact_size(
                                        egui::vec2(34.0, 17.0),
                                        egui::Sense::hover(),
                                    );
                                    let p = ui.painter();
                                    p.rect_filled(r, 4.0, egui::Color32::from_rgb(88, 68, 34));
                                    p.rect_stroke(
                                        r,
                                        4.0,
                                        egui::Stroke::new(1.0, GOLD_DIM),
                                        egui::StrokeKind::Inside,
                                    );
                                    p.text(
                                        r.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "置顶",
                                        egui::FontId::proportional(10.5),
                                        GOLD,
                                    );
                                }
                                ui.label(
                                    egui::RichText::new(&n.title)
                                        .size(14.5)
                                        .strong()
                                        .color(egui::Color32::from_rgb(240, 228, 200)),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(
                                                n.created_at.split(' ').next().unwrap_or(""),
                                            )
                                            .size(11.0)
                                            .color(INK_WEAK),
                                        );
                                    },
                                );
                            });
                            if !n.body.is_empty() {
                                ui.add_space(3.0);
                                ui.label(egui::RichText::new(&n.body).size(12.5).color(INK_WEAK));
                            }
                        });
                        ui.add_space(8.0);
                    }
                });
        });
    }

    fn draw_auth(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(238.0);
            ui.vertical(|ui| match self.logged.clone() {
                Some(user) => self.draw_account(ui, &user),
                None => self.draw_form(ui),
            });
        });
    }

    /// 已登录: 用户信息 + 可点击的区服行 + 进入游戏/立即更新
    fn draw_account(&mut self, ui: &mut egui::Ui, user: &str) {
        let srv_name = self.server().name.clone();
        let srv_game = self.server().game.clone();
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            // 头像位: 账号首字母金圈
            let (r, _) = ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
            let p = ui.painter();
            p.circle_filled(r.center(), 18.0, egui::Color32::from_rgb(58, 46, 26));
            p.circle_stroke(r.center(), 18.0, egui::Stroke::new(1.2, GOLD_DIM));
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                user.chars()
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
                egui::FontId::proportional(18.0),
                GOLD,
            );
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(user).size(15.5).strong().color(INK));
                ui.label(egui::RichText::new("已登录").size(11.5).color(GOLD_DIM));
            });
        });
        ui.add_space(10.0);
        // 区服行: 整行可点, 弹出服务器列表
        {
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::click());
            let p = ui.painter();
            let hovered = resp.hovered();
            p.rect_filled(
                rect,
                8.0,
                if hovered {
                    egui::Color32::from_rgb(48, 40, 26)
                } else {
                    PANEL_2
                },
            );
            p.rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.0, if hovered { GOLD_DIM } else { EDGE }),
                egui::StrokeKind::Inside,
            );
            p.text(
                egui::pos2(rect.left() + 12.0, rect.top() + 14.0),
                egui::Align2::LEFT_CENTER,
                "当前区服",
                egui::FontId::proportional(11.0),
                INK_WEAK,
            );
            p.text(
                egui::pos2(rect.left() + 12.0, rect.bottom() - 15.0),
                egui::Align2::LEFT_CENTER,
                &srv_name,
                egui::FontId::proportional(14.5),
                GOLD,
            );
            let hint = if hovered { GOLD } else { INK_WEAK };
            p.text(
                egui::pos2(rect.right() - 22.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                "切换",
                egui::FontId::proportional(11.5),
                hint,
            );
            // 右箭头 (msyh 无 ▸ 字形, 画三角代替)
            let ac = egui::pos2(rect.right() - 14.0, rect.center().y);
            p.add(egui::Shape::convex_polygon(
                vec![
                    ac + egui::vec2(-2.0, -4.0),
                    ac + egui::vec2(3.0, 0.0),
                    ac + egui::vec2(-2.0, 4.0),
                ],
                hint,
                egui::Stroke::NONE,
            ));
            if hovered {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                self.show_server_pick = true;
            }
        }
        ui.label(egui::RichText::new(&srv_game).size(10.5).color(INK_WEAK));
        ui.add_space(10.0);
        // 主按钮: 所选区服需要更新时变身「立即更新」
        let mut do_update = false;
        match &self.update_state {
            UpdateState::Available(n, bytes) => {
                if gold_button(
                    ui,
                    "立 即 更 新",
                    egui::vec2(ui.available_width(), 42.0),
                    true,
                )
                .clicked()
                {
                    do_update = true;
                }
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(format!(
                        "新版本: {n} 个文件 · {:.1} MB",
                        *bytes as f64 / 1048576.0
                    ))
                    .size(11.0)
                    .color(INK_WEAK),
                );
            }
            UpdateState::Downloading(..) => {
                let _ = gold_button(
                    ui,
                    "更 新 中 ...",
                    egui::vec2(ui.available_width(), 42.0),
                    false,
                );
            }
            _ => {
                let can = !self.busy;
                if gold_button(
                    ui,
                    "进 入 游 戏",
                    egui::vec2(ui.available_width(), 42.0),
                    can,
                )
                .clicked()
                    && can
                {
                    // 票据 60 秒即失效, 故点击时才重新登录换取新票据
                    self.pending_launch = true;
                    self.auth(AuthAction::Login {
                        user: self.user.clone(),
                        pass: self.pass.clone(),
                    });
                }
            }
        }
        if do_update {
            self.start_update();
        }
        ui.add_space(4.0);
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), 26.0),
                egui::Button::new(egui::RichText::new("切换账号").size(12.5).color(INK_WEAK))
                    .frame(false),
            )
            .clicked()
        {
            self.logged = None;
            self.pass.clear();
            self.status.clear();
            self.tab = Tab::Login;
        }
        if self.busy {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("正在进入...")
                        .size(12.5)
                        .color(INK_WEAK),
                );
            });
        } else if !self.status.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&self.status).size(12.5).color(GOLD));
        }
    }

    fn draw_form(&mut self, ui: &mut egui::Ui) {
        let mut sel = match self.tab {
            Tab::Login => 0usize,
            Tab::Register => 1,
            Tab::Reset => 2,
        };
        if segmented(ui, 238.0, &["登录", "注册", "找回密码"], &mut sel) {
            self.tab = [Tab::Login, Tab::Register, Tab::Reset][sel];
            self.status.clear();
        }
        ui.add_space(if matches!(self.tab, Tab::Register) {
            8.0
        } else {
            10.0
        });
        // 注册页 5 个字段, 常规密度会高过主区压到底部状态条 — 用紧凑档
        let (vmargin, fgap) = if matches!(self.tab, Tab::Register) {
            (4, 1.0)
        } else {
            (7, 4.0)
        };
        let field = |ui: &mut egui::Ui, label: &str, buf: &mut String, pw: bool| {
            ui.label(egui::RichText::new(label).size(12.0).color(INK_WEAK));
            ui.add_space(1.0);
            ui.add(
                egui::TextEdit::singleline(buf)
                    .password(pw)
                    .desired_width(f32::INFINITY)
                    .font(egui::FontId::proportional(14.0))
                    .margin(egui::Margin::symmetric(10, vmargin)),
            );
            ui.add_space(fgap);
        };
        field(ui, "账号", &mut self.user, false);
        match self.tab {
            Tab::Login => {
                field(ui, "密码", &mut self.pass, true);
            }
            Tab::Register => {
                field(ui, "密码", &mut self.pass, true);
                field(ui, "确认密码", &mut self.pass2, true);
                field(ui, "密保问题 (选填, 找回用)", &mut self.question, false);
                field(ui, "密保答案", &mut self.answer, false);
            }
            Tab::Reset => {
                field(ui, "密保答案", &mut self.answer, false);
                field(ui, "新密码", &mut self.pass, true);
            }
        }
        ui.add_space(if matches!(self.tab, Tab::Register) {
            6.0
        } else {
            8.0
        });
        let label = match self.tab {
            Tab::Login => "登 录",
            Tab::Register => "注 册",
            Tab::Reset => "重 设 密 码",
        };
        let can = !self.busy && !self.user.is_empty();
        let btn_h = if matches!(self.tab, Tab::Register) {
            36.0
        } else {
            40.0
        };
        if gold_button(ui, label, egui::vec2(ui.available_width(), btn_h), can).clicked() && can {
            match self.tab {
                Tab::Login => self.auth(AuthAction::Login {
                    user: self.user.clone(),
                    pass: self.pass.clone(),
                }),
                Tab::Register => {
                    if self.pass != self.pass2 {
                        self.status = "两次密码不一致".into();
                    } else {
                        self.auth(AuthAction::Register {
                            user: self.user.clone(),
                            pass: self.pass.clone(),
                            question: self.question.clone(),
                            answer: self.answer.clone(),
                        });
                    }
                }
                Tab::Reset => self.auth(AuthAction::Reset {
                    user: self.user.clone(),
                    answer: self.answer.clone(),
                    new_pass: self.pass.clone(),
                }),
            }
        }
        if self.busy {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("处理中...").size(12.5).color(INK_WEAK));
            });
        } else if !self.status.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(&self.status).size(12.5).color(GOLD));
        }
    }

    /// 弹层公共壳: 半透明遮罩 (点击关闭) + 居中金边卡片
    fn popup_shell(
        ctx: &egui::Context,
        id: &str,
        size: egui::Vec2,
        add: impl FnOnce(&mut egui::Ui) -> bool,
    ) -> bool {
        let mut close = false;
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new(id))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.left_top())
            .show(ctx, |ui| {
                // 遮罩: 圆角同窗体, 点击空白处关闭
                let mask = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    WIN_RADIUS,
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150),
                );
                if mask.clicked() {
                    close = true;
                }
                let rect = egui::Rect::from_center_size(screen.center(), size);
                let card = ui.allocate_rect(rect, egui::Sense::click()); // 挡住遮罩点击
                let _ = card;
                let p = ui.painter();
                p.rect_filled(rect, 12.0, PANEL);
                p.rect_stroke(
                    rect,
                    12.0,
                    egui::Stroke::new(1.2, GOLD_DIM),
                    egui::StrokeKind::Inside,
                );
                let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(18.0)));
                if add(&mut inner) {
                    close = true;
                }
            });
        close
    }

    /// 设置弹层: 分辨率 + 全屏
    fn draw_settings_popup(&mut self, ctx: &egui::Context) {
        let mut window = self.settings.window.clone();
        let mut fullscreen = self.settings.fullscreen;
        let mut changed = false;
        let close = Self::popup_shell(ctx, "settings-pop", egui::vec2(300.0, 170.0), |ui| {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("设 置").size(16.0).strong().color(GOLD));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("×").size(15.0)).frame(false))
                        .clicked()
                    {
                        done = true;
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("分辨率").size(13.0).color(INK_WEAK));
                ui.add_space(8.0);
                egui::ComboBox::from_id_salt("res")
                    .selected_text(egui::RichText::new(&window).size(13.0))
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for r in ["1280x720", "1600x900", "1920x1080"] {
                            if ui.selectable_label(window == r, r).clicked() {
                                window = r.into();
                                changed = true;
                            }
                        }
                    });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("显示").size(13.0).color(INK_WEAK));
                ui.add_space(22.0);
                if ui
                    .checkbox(&mut fullscreen, egui::RichText::new("全屏运行").size(13.0))
                    .changed()
                {
                    changed = true;
                }
            });
            done
        });
        if changed {
            self.settings.window = window;
            self.settings.fullscreen = fullscreen;
            save_settings(&self.settings);
        }
        if close {
            self.show_settings = false;
        }
    }

    /// 区服选择弹层: 列出全部服务器, 点选即切换
    fn draw_server_popup(&mut self, ctx: &egui::Context) {
        let servers = self.servers.clone();
        let cur = self.settings.last_server;
        let mut picked: Option<usize> = None;
        let h = 92.0 + servers.len().min(6) as f32 * 54.0;
        let close = Self::popup_shell(ctx, "server-pop", egui::vec2(340.0, h), |ui| {
            let mut done = false;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("选 择 区 服")
                        .size(16.0)
                        .strong()
                        .color(GOLD),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new(egui::RichText::new("×").size(15.0)).frame(false))
                        .clicked()
                    {
                        done = true;
                    }
                });
            });
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .max_height(6.0 * 54.0)
                .show(ui, |ui| {
                    for (idx, srv) in servers.iter().enumerate() {
                        let selected = idx == cur;
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 48.0),
                            egui::Sense::click(),
                        );
                        let p = ui.painter();
                        let hov = resp.hovered();
                        p.rect_filled(
                            rect,
                            8.0,
                            if selected {
                                egui::Color32::from_rgb(52, 42, 24)
                            } else if hov {
                                egui::Color32::from_rgb(46, 38, 25)
                            } else {
                                PANEL_2
                            },
                        );
                        p.rect_stroke(
                            rect,
                            8.0,
                            egui::Stroke::new(1.0, if selected { GOLD_DIM } else { EDGE }),
                            egui::StrokeKind::Inside,
                        );
                        p.text(
                            egui::pos2(rect.left() + 12.0, rect.top() + 14.0),
                            egui::Align2::LEFT_CENTER,
                            &srv.name,
                            egui::FontId::proportional(13.5),
                            if selected { GOLD } else { INK },
                        );
                        p.text(
                            egui::pos2(rect.left() + 12.0, rect.bottom() - 13.0),
                            egui::Align2::LEFT_CENTER,
                            &srv.game,
                            egui::FontId::proportional(10.5),
                            INK_WEAK,
                        );
                        if selected {
                            p.text(
                                egui::pos2(rect.right() - 12.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                "✔ 当前",
                                egui::FontId::proportional(11.5),
                                GOLD,
                            );
                        }
                        if hov {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            picked = Some(idx);
                            done = true;
                        }
                        ui.add_space(6.0);
                    }
                });
            done
        });
        if let Some(idx) = picked {
            if idx != self.settings.last_server {
                self.settings.last_server = idx;
                save_settings(&self.settings);
                // 切服后重取该服公告与更新计划
                self.news.clear();
                self.update_state = UpdateState::Checking;
                self.refresh_remote();
            }
        }
        if close {
            self.show_server_pick = false;
        }
    }

    fn draw_update_bar(&mut self, ui: &mut egui::Ui) {
        let mut do_retry = false;
        let r = ui.max_rect();
        let y = ui.cursor().top();
        ui.painter().line_segment(
            [egui::pos2(r.left(), y), egui::pos2(r.right(), y)],
            egui::Stroke::new(1.0, EDGE),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            match &self.update_state {
                UpdateState::Checking => {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("检查更新中...")
                            .size(12.5)
                            .color(INK_WEAK),
                    );
                }
                UpdateState::UpToDate => {
                    ui.label(egui::RichText::new("●").size(11.0).color(GOLD));
                    ui.label(
                        egui::RichText::new("客户端已是最新版本")
                            .size(12.5)
                            .color(INK_WEAK),
                    );
                }
                UpdateState::Available(n, bytes) => {
                    ui.label(egui::RichText::new("●").size(11.0).color(GOLD));
                    ui.label(
                        egui::RichText::new(format!(
                            "发现新版本 — {n} 个文件 · {:.1} MB",
                            *bytes as f64 / 1048576.0
                        ))
                        .size(12.5)
                        .color(INK),
                    );
                    ui.label(
                        egui::RichText::new("登录后在账号面板更新")
                            .size(11.5)
                            .color(INK_WEAK),
                    );
                }
                UpdateState::Downloading(done, total, file) => {
                    let frac = if *total > 0 {
                        *done as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(300.0)
                            .desired_height(16.0)
                            .corner_radius(8.0)
                            .fill(GOLD_DIM),
                    );
                    ui.label(
                        egui::RichText::new(format!("{done}/{total}  {file}"))
                            .size(11.5)
                            .color(INK_WEAK),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(200));
                }
                UpdateState::Failed(e) => {
                    ui.label(egui::RichText::new("●").size(11.0).color(DANGER));
                    ui.label(egui::RichText::new(e).size(12.5).color(DANGER));
                    ui.add_space(4.0);
                    if ui
                        .add(egui::Button::new(egui::RichText::new("重试").size(12.5)))
                        .clicked()
                    {
                        do_retry = true;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("MirForge · 开源引擎")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(96, 86, 70)),
                );
            });
        });
        if do_retry {
            self.refresh_remote();
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([880.0, 560.0])
            .with_min_inner_size([760.0, 500.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_title("MirForge 登录器"),
        ..Default::default()
    };
    eframe::run_native(
        "MirForge Launcher",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
