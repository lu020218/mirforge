//! 音频: packs/sound/ 下的 .ogg (转码脚本产物, 与图像 packs 同一自管理路线,
//! 不走 bevy assets 目录) — SoundBank 惰性缓存 + Sfx 事件队列 + BGM 循环。
//!
//! 音量: MIRFORGE_SFX_VOL / MIRFORGE_BGM_VOL 环境变量可调 (0..1);
//! 设置面板滑杆与持久化留待后续 (P3)。

use bevy::audio::{AudioPlayer, AudioSource, PlaybackSettings, Volume};
use bevy::prelude::*;
use sim::layout::hum;
use std::collections::HashMap;
use std::path::PathBuf;

/// 世界音效可闻半径 (格); 超出不播, 半径内线性衰减
pub const HEAR_RANGE: f64 = 12.0;

/// 播放请求: name = packs/sound/<name>.ogg (不带扩展名)
#[derive(Event)]
pub struct Sfx {
    pub name: String,
    pub vol: f32,
}

impl Sfx {
    /// 世界音效: 按与玩家的格距线性衰减; 超出可闻半径返回 None
    pub fn at(name: impl Into<String>, dist: f64) -> Option<Self> {
        let k = 1.0 - dist / HEAR_RANGE;
        (k > 0.02).then(|| Sfx {
            name: name.into(),
            vol: k as f32,
        })
    }

    /// 界面音效: 不衰减
    pub fn ui(name: impl Into<String>) -> Self {
        Sfx {
            name: name.into(),
            vol: 1.0,
        }
    }
}

/// 音效库: 名字 → 解码源句柄 (fs 直读构造 AudioSource, 缺文件缓存 None 只探一次)
#[derive(Resource)]
pub struct SoundBank {
    root: PathBuf,
    cache: HashMap<String, Option<Handle<AudioSource>>>,
    pub sfx_vol: f32,
    pub bgm_vol: f32,
}

impl SoundBank {
    pub fn new() -> Self {
        let packs = std::env::var("MIRFORGE_PACKS").unwrap_or_else(|_| "packs".into());
        let vol = |k: &str, d: f32| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v.clamp(0.0, 1.0))
                .unwrap_or(d)
        };
        Self {
            root: PathBuf::from(packs).join("sound"),
            cache: HashMap::new(),
            sfx_vol: vol("MIRFORGE_SFX_VOL", 0.9),
            bgm_vol: vol("MIRFORGE_BGM_VOL", 0.5),
        }
    }

    fn get(&mut self, audio: &mut Assets<AudioSource>, name: &str) -> Option<Handle<AudioSource>> {
        if !self.cache.contains_key(name) {
            let h = std::fs::read(self.root.join(format!("{name}.ogg")))
                .ok()
                .map(|bytes| audio.add(AudioSource {
                    bytes: bytes.into(),
                }));
            self.cache.insert(name.to_string(), h);
        }
        self.cache.get(name).and_then(|h| h.clone())
    }
}

/// BGM 状态: want 由 bgm_pick 按界面/区域决定, cur 是正在循环的实体
#[derive(Resource, Default)]
pub struct Bgm {
    pub want: Option<String>,
    cur: Option<(String, Entity)>,
}

/// 排空 Sfx 事件 + BGM 换曲 (直切; 淡入淡出留待后续)
pub fn audio_step(
    mut commands: Commands,
    mut audio: ResMut<Assets<AudioSource>>,
    mut bank: ResMut<SoundBank>,
    mut evs: EventReader<Sfx>,
    mut bgm: ResMut<Bgm>,
) {
    for e in evs.read() {
        let v = e.vol * bank.sfx_vol;
        if v <= 0.01 {
            continue;
        }
        if let Some(h) = bank.get(&mut audio, &e.name) {
            debug!("sfx {} vol {:.2}", e.name, v);
            commands.spawn((
                AudioPlayer(h),
                PlaybackSettings {
                    volume: Volume::new(v),
                    ..PlaybackSettings::DESPAWN
                },
            ));
        }
    }
    let cur_name = bgm.cur.as_ref().map(|(n, _)| n.clone());
    if bgm.want != cur_name {
        if let Some((_, ent)) = bgm.cur.take() {
            commands.entity(ent).despawn();
        }
        if let Some(name) = bgm.want.clone() {
            if let Some(h) = bank.get(&mut audio, &format!("bgm/{name}")) {
                let vol = bank.bgm_vol;
                let ent = commands
                    .spawn((
                        AudioPlayer(h),
                        PlaybackSettings {
                            volume: Volume::new(vol),
                            ..PlaybackSettings::LOOP
                        },
                    ))
                    .id();
                bgm.cur = Some((name, ent));
            }
        }
    }
}

/// 想播哪首: 登录/选角 → login 主题曲; 游戏内 → 区域 BGM (服务器随切区下发)
pub fn bgm_pick(
    screen: Res<State<crate::Screen>>,
    net: Res<crate::Net>,
    mut bgm: ResMut<Bgm>,
) {
    let want = match screen.get() {
        crate::Screen::Boot => None,
        crate::Screen::Login | crate::Screen::CharSelect => Some("login".to_string()),
        crate::Screen::InGame => (!net.zone_bgm.is_empty()).then(|| net.zone_bgm.clone()),
    };
    if bgm.want != want {
        bgm.want = want;
    }
}

/// 本地玩家脚步 + 普攻挥砍 (施法音走 SkillEffect 广播, 自己也会收到)
pub fn self_sounds(
    time: Res<Time>,
    q: Query<&crate::Player>,
    mut ev: EventWriter<Sfx>,
    mut next_step: Local<f64>,
    mut left: Local<bool>,
    mut last_atk: Local<Option<f64>>,
) {
    let Ok(p) = q.get_single() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    if p.moving {
        if now >= *next_step {
            let name = match (p.running, *left) {
                (false, true) => "hum/walk_l",
                (false, false) => "hum/walk_r",
                (true, true) => "hum/run_l",
                (true, false) => "hum/run_r",
            };
            ev.send(Sfx {
                name: name.into(),
                vol: 0.7,
            });
            *left = !*left;
            *next_step = now + if p.running { 0.28 } else { 0.42 };
        }
    } else {
        *next_step = now; // 停步重置, 再迈步立刻响第一脚
    }
    if p.attack_start != *last_atk {
        *last_atk = p.attack_start;
        if p.attack_start.is_some() && p.attack_base == hum::ATTACK {
            ev.send(Sfx {
                name: "hum/swing".into(),
                vol: 0.9,
            });
        }
    }
}
