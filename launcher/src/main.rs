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

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("MirForge");
                ui.separator();
                ui.label("服务器:");
                let mut sel = self.settings.last_server;
                egui::ComboBox::from_id_salt("srv")
                    .selected_text(&self.servers[sel].name)
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
            });
        });

        egui::SidePanel::right("auth")
            .exact_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Login, "登录");
                    ui.selectable_value(&mut self.tab, Tab::Register, "注册");
                    ui.selectable_value(&mut self.tab, Tab::Reset, "找回密码");
                });
                ui.separator();
                ui.label("账号");
                ui.text_edit_singleline(&mut self.user);
                match self.tab {
                    Tab::Login => {
                        ui.label("密码");
                        ui.add(egui::TextEdit::singleline(&mut self.pass).password(true));
                    }
                    Tab::Register => {
                        ui.label("密码");
                        ui.add(egui::TextEdit::singleline(&mut self.pass).password(true));
                        ui.label("确认密码");
                        ui.add(egui::TextEdit::singleline(&mut self.pass2).password(true));
                        ui.label("密保问题 (选填, 找回密码用)");
                        ui.text_edit_singleline(&mut self.question);
                        ui.label("密保答案");
                        ui.text_edit_singleline(&mut self.answer);
                    }
                    Tab::Reset => {
                        ui.label("密保答案");
                        ui.text_edit_singleline(&mut self.answer);
                        ui.label("新密码");
                        ui.add(egui::TextEdit::singleline(&mut self.pass).password(true));
                    }
                }
                ui.add_space(8.0);
                let label = match self.tab {
                    Tab::Login => "登录并开始游戏",
                    Tab::Register => "注册并开始游戏",
                    Tab::Reset => "重设密码",
                };
                let can = !self.busy && !self.user.is_empty();
                if ui.add_enabled(can, egui::Button::new(label)).clicked() {
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
                if !self.status.is_empty() {
                    ui.add_space(4.0);
                    ui.colored_label(egui::Color32::from_rgb(230, 180, 90), &self.status);
                }
                ui.separator();
                ui.label("游戏设置");
                ui.horizontal(|ui| {
                    ui.label("分辨率:");
                    egui::ComboBox::from_id_salt("res")
                        .selected_text(&self.settings.window)
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

        egui::TopBottomPanel::bottom("update").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| match &self.update_state {
                UpdateState::Checking => {
                    ui.spinner();
                    ui.label("检查更新中...");
                }
                UpdateState::UpToDate => {
                    ui.label("✔ 客户端已是最新");
                }
                UpdateState::Available(n, bytes) => {
                    ui.label(format!(
                        "发现更新: {n} 个文件, {:.1} MB",
                        *bytes as f64 / 1048576.0
                    ));
                    if ui.button("立即更新").clicked() {
                        self.start_update();
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
                            .desired_width(260.0)
                            .text(format!("{done}/{total} {file}")),
                    );
                    ctx.request_repaint_after(std::time::Duration::from_millis(200));
                }
                UpdateState::Failed(e) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 120, 100), e);
                    if ui.button("重试").clicked() {
                        self.refresh_remote();
                    }
                }
            });
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("新闻公告");
            ui.add_space(4.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.news.is_empty() {
                    ui.weak("暂无公告 (服务器未发布或不可达)");
                }
                for n in &self.news {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if n.pinned {
                                ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "[置顶]");
                            }
                            ui.strong(&n.title);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| ui.weak(&n.created_at),
                            );
                        });
                        if !n.body.is_empty() {
                            ui.label(&n.body);
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([860.0, 540.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("MirForge 登录器"),
        ..Default::default()
    };
    eframe::run_native(
        "MirForge Launcher",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
