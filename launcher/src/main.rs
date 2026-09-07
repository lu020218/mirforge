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
                    if ok {
                        if let Some(t) = ticket {
                            self.settings.username = self.user.clone();
                            save_settings(&self.settings);
                            self.ticket = Some(t.clone());
                            self.launch_game(&t);
                        } else {
                            self.tab = Tab::Login; // 找回成功回登录页
                        }
                    }
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
                // 窗体: 圆角深底 + 金边
                painter.rect_filled(rect, 14.0, BG);
                painter.rect_stroke(
                    rect.shrink(0.5),
                    14.0,
                    egui::Stroke::new(1.2, EDGE),
                    egui::StrokeKind::Inside,
                );
                let inner = rect.shrink(16.0);
                let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                self.draw_header(ctx, &mut ui);
                ui.add_space(10.0);
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
                            self.draw_auth(ui);
                        });
                    });
                });
                ui.add_space(8.0);
                self.draw_update_bar(&mut ui);
            });
    }
}

impl App {
    /// 自绘标题栏: 左 logo + 服务器选择, 右最小化/关闭; 空白区可拖动
    fn draw_header(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let bar = ui
            .horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new("Mir").size(24.0).strong().color(GOLD));
                ui.label(egui::RichText::new("Forge").size(24.0).strong().color(INK));
                ui.add_space(2.0);
                ui.label(egui::RichText::new("登 录 器").size(12.0).color(INK_WEAK));
                ui.add_space(18.0);
                ui.label(egui::RichText::new("线路").color(INK_WEAK));
                let mut sel = self.settings.last_server;
                egui::ComboBox::from_id_salt("srv")
                    .selected_text(egui::RichText::new(&self.servers[sel].name).color(GOLD))
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for (i, s) in self.servers.iter().enumerate() {
                            ui.selectable_value(&mut sel, i, &s.name);
                        }
                    });
                if sel != self.settings.last_server {
                    self.settings.last_server = sel;
                    save_settings(&self.settings);
                    self.news.clear();
                    self.refresh_remote();
                }
                // 右侧窗控
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let btn = |ui: &mut egui::Ui, t: &str| {
                        ui.add(
                            egui::Button::new(egui::RichText::new(t).size(15.0).color(INK_WEAK))
                                .frame(false)
                                .min_size(egui::vec2(30.0, 26.0)),
                        )
                    };
                    if btn(ui, "×").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if btn(ui, "—").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                });
            })
            .response;
        // 标题栏空白拖动窗口
        let drag = ui.interact(
            bar.rect,
            egui::Id::new("titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        ui.add_space(6.0);
        // 金色分隔线
        let r = ui.max_rect();
        let y = ui.cursor().top();
        ui.painter().line_segment(
            [egui::pos2(r.left(), y), egui::pos2(r.right(), y)],
            egui::Stroke::new(1.0, EDGE),
        );
    }

    fn draw_news(&mut self, ui: &mut egui::Ui, h: f32) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("新闻公告")
                        .size(17.0)
                        .strong()
                        .color(GOLD),
                );
            });
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .max_height(h - 30.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.news.is_empty() {
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("暂无公告").color(INK_WEAK));
                    }
                    for n in &self.news {
                        card().show(ui, |ui| {
                            ui.set_width(ui.available_width() - 6.0);
                            ui.horizontal(|ui| {
                                if n.pinned {
                                    ui.label(
                                        egui::RichText::new("置顶")
                                            .size(11.0)
                                            .color(BG)
                                            .background_color(GOLD),
                                    );
                                }
                                ui.label(egui::RichText::new(&n.title).strong().color(INK));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(&n.created_at)
                                                .small()
                                                .color(INK_WEAK),
                                        );
                                    },
                                );
                            });
                            if !n.body.is_empty() {
                                ui.add_space(2.0);
                                ui.label(egui::RichText::new(&n.body).size(13.0).color(INK_WEAK));
                            }
                        });
                        ui.add_space(6.0);
                    }
                });
        });
    }

    fn draw_auth(&mut self, ui: &mut egui::Ui) {
        card().show(ui, |ui| {
            ui.set_width(238.0);
            ui.vertical(|ui| {
                // 页签
                ui.horizontal(|ui| {
                    for (t, name) in [
                        (Tab::Login, "登录"),
                        (Tab::Register, "注册"),
                        (Tab::Reset, "找回密码"),
                    ] {
                        let sel = self.tab == t;
                        let text = egui::RichText::new(name).size(14.5).color(if sel {
                            GOLD
                        } else {
                            INK_WEAK
                        });
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            self.tab = t;
                            self.status.clear();
                        }
                        if sel {
                            let r = ui.min_rect();
                            ui.painter().line_segment(
                                [
                                    egui::pos2(r.right() - 34.0, r.bottom() + 2.0),
                                    egui::pos2(r.right() - 6.0, r.bottom() + 2.0),
                                ],
                                egui::Stroke::new(2.0, GOLD),
                            );
                        }
                    }
                });
                ui.add_space(8.0);
                let field = |ui: &mut egui::Ui, label: &str, buf: &mut String, pw: bool| {
                    ui.label(egui::RichText::new(label).size(12.5).color(INK_WEAK));
                    ui.add(
                        egui::TextEdit::singleline(buf)
                            .password(pw)
                            .desired_width(f32::INFINITY)
                            .margin(egui::Margin::symmetric(8, 6)),
                    );
                    ui.add_space(2.0);
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
                ui.add_space(8.0);
                let label = match self.tab {
                    Tab::Login => "进 入 游 戏",
                    Tab::Register => "注 册 并 进 入",
                    Tab::Reset => "重 设 密 码",
                };
                let can = !self.busy && !self.user.is_empty();
                let btn =
                    egui::Button::new(egui::RichText::new(label).size(16.0).strong().color(BG))
                        .fill(if can { GOLD } else { GOLD_DIM })
                        .corner_radius(8.0)
                        .min_size(egui::vec2(ui.available_width(), 38.0));
                if ui.add_enabled(can, btn).clicked() {
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
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("处理中...").color(INK_WEAK));
                    });
                } else if !self.status.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&self.status).color(GOLD));
                }
                ui.add_space(10.0);
                let r = ui.min_rect();
                ui.painter().line_segment(
                    [
                        egui::pos2(r.left(), ui.cursor().top()),
                        egui::pos2(r.right(), ui.cursor().top()),
                    ],
                    egui::Stroke::new(1.0, EDGE),
                );
                ui.add_space(6.0);
                ui.label(egui::RichText::new("游戏设置").size(13.0).color(INK_WEAK));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("分辨率").size(12.5).color(INK_WEAK));
                    egui::ComboBox::from_id_salt("res")
                        .selected_text(&self.settings.window)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for r in ["1280x720", "1600x900", "1920x1080"] {
                                if ui.selectable_label(self.settings.window == r, r).clicked() {
                                    self.settings.window = r.into();
                                    save_settings(&self.settings);
                                }
                            }
                        });
                });
                if ui
                    .checkbox(&mut self.settings.fullscreen, "全屏 (无边框)")
                    .changed()
                {
                    save_settings(&self.settings);
                }
            });
        });
    }

    fn draw_update_bar(&mut self, ui: &mut egui::Ui) {
        let mut do_update = false;
        let mut do_retry = false;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width() - 6.0);
            ui.horizontal(|ui| match &self.update_state {
                UpdateState::Checking => {
                    ui.spinner();
                    ui.label(egui::RichText::new("检查更新中...").color(INK_WEAK));
                }
                UpdateState::UpToDate => {
                    ui.label(egui::RichText::new("✔").color(GOLD));
                    ui.label(egui::RichText::new("客户端已是最新").color(INK_WEAK));
                }
                UpdateState::Available(n, bytes) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "发现更新: {n} 个文件, {:.1} MB",
                            *bytes as f64 / 1048576.0
                        ))
                        .color(INK),
                    );
                    let b = egui::Button::new(egui::RichText::new("立即更新").color(BG).strong())
                        .fill(GOLD)
                        .corner_radius(6.0);
                    if ui.add(b).clicked() {
                        do_update = true;
                    }
                }
                UpdateState::Downloading(done, total, file) => {
                    let frac = if *total > 0 {
                        *done as f32 / *total as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(frac)
                            .desired_width(320.0)
                            .fill(GOLD_DIM)
                            .text(
                                egui::RichText::new(format!("{done}/{total}  {file}")).size(12.0),
                            ),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(200));
                }
                UpdateState::Failed(e) => {
                    ui.label(egui::RichText::new(e).color(DANGER));
                    if ui.button("重试").clicked() {
                        do_retry = true;
                    }
                }
            });
        });
        if do_update {
            self.start_update();
        }
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
