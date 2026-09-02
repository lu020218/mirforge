//! 开发钩子: 自动登录/对话/截图(自 main.rs 机械拆出)。
use crate::*;

/// 开发钩子: MIRFORGE_TALK=npc_id[:选项下标] — 进图后自动搭话并选一项
///
/// UI 自查用: 对话框/商店窗都靠点 NPC 才出得来, 用鼠标脚本瞄精灵很不稳。
pub(crate) fn dev_talk(
    net: ResMut<Net>,
    time: Res<Time>,
    mut stage: Local<u8>,
    mut seen_dialog: Local<u32>,
) {
    let Ok(cfg) = std::env::var("MIRFORGE_TALK") else {
        return;
    };
    let (npc, opt) = match cfg.split_once(':') {
        Some((n, i)) => (n.to_string(), i.parse::<u32>().ok()),
        None => (cfg, None),
    };
    // 等进图站稳再开口
    if *stage == 0 && time.elapsed_secs_f64() > 6.0 && !net.npcs.is_empty() {
        *stage = 1;
        net.send(ClientMessage::TalkNpc { npc_id: npc });
        return;
    }
    if *stage == 1 {
        if let (Some(idx), Some(d)) = (opt, net.dialog.as_ref()) {
            if *seen_dialog != net.dialog_rev {
                *seen_dialog = net.dialog_rev;
                *stage = 2;
                net.send(ClientMessage::NpcOption {
                    npc_id: d.npc_id.clone(),
                    page: d.page,
                    idx,
                });
            }
        }
    }
}

/// 开发钩子: MIRFORGE_SHOT=路径[,延迟秒] — 延迟后截图存盘并退出 (视觉回归自查用)
pub(crate) fn dev_screenshot(
    mut commands: Commands,
    time: Res<Time>,
    mut done: Local<bool>,
    mut exit: EventWriter<AppExit>,
    mut shot_at: Local<f64>,
) {
    let Ok(cfg) = std::env::var("MIRFORGE_SHOT") else {
        return;
    };
    let (path, delay) = match cfg.split_once(',') {
        Some((p, d)) => (p.to_string(), d.parse().unwrap_or(8.0)),
        None => (cfg, 8.0),
    };
    let t = time.elapsed_secs_f64();
    if !*done {
        if t < delay {
            return;
        }
        *done = true;
        *shot_at = t;
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(path));
    } else if t > *shot_at + 1.5 {
        // 留一帧余量让文件落盘
        exit.send(AppExit::Success);
    }
}

/// 开发钩子: MIRFORGE_AUTOLOGIN=user:pass 自动 注册→(已存在则登录)→建角→选角
/// (联调/自动化测试用; 角色名 = 用户名)
pub(crate) fn dev_autologin(
    time: Res<Time>,
    mut net: ResMut<Net>,
    screen: Res<State<Screen>>,
    mut stage: Local<u8>,
    mut wait_since: Local<f64>,
) {
    let Ok(cred) = std::env::var("MIRFORGE_AUTOLOGIN") else {
        return;
    };
    let mut it = cred.splitn(3, ':');
    let (Some(user), Some(pass)) = (it.next(), it.next()) else {
        return;
    };
    let class = match it.next() {
        Some("mage") => CharacterClass::Mage,
        Some("taoist") => CharacterClass::Taoist,
        _ => CharacterClass::Warrior,
    };
    match screen.get() {
        Screen::Login if net.connected => {
            if *stage == 0 {
                *stage = 1;
                net.send(ClientMessage::Register {
                    username: user.into(),
                    password: pass.into(),
                });
            } else if *stage == 1 && net.status == "用户名已存在" {
                *stage = 2;
                net.send(ClientMessage::Login {
                    username: user.into(),
                    password: pass.into(),
                });
            }
        }
        Screen::CharSelect => {
            if *wait_since == 0.0 {
                *wait_since = time.elapsed_secs_f64();
            }
            if *stage >= 5 {
                return;
            }
            if let Some(c) = net.characters.iter().find(|c| c.name == user) {
                *stage = 5;
                let id = c.id.clone();
                net.character_id = Some(id.clone());
                net.send(ClientMessage::SelectCharacter { character_id: id });
            } else if *stage < 4 && time.elapsed_secs_f64() - *wait_since > 1.0 {
                *stage = 4;
                net.send(ClientMessage::CreateCharacter {
                    name: user.into(),
                    class,
                    gender: "male".into(),
                });
            }
        }
        _ => {}
    }
}
