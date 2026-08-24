//! 技能冒烟：法师进图 → 收技能表 → 远程火球命中稻草人 → 断言 特效/伤害/扣蓝/冷却。
//! 用法: 先起服务器, 再 `cargo run -p mirforge-server --example skills`

use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMessage, Position, ServerMessage, PROTOCOL_VERSION};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Ws, msg: &ClientMessage) {
    ws.send(Message::Text(protocol::encode(msg).unwrap()))
        .await
        .unwrap();
}

async fn recv_until<F: Fn(&ServerMessage) -> bool>(ws: &mut Ws, what: &str, f: F) -> ServerMessage {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("等待 {what} 超时"))
            .expect("连接中断")
            .unwrap();
        if let Message::Text(text) = msg {
            if let Ok(m) = protocol::decode_server(&text) {
                if f(&m) {
                    return m;
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("MIRFORGE_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".into());
    let url = format!("ws://{addr}");
    let user = format!("mage_{}", std::process::id());

    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("连不上服务器");
    send(
        &mut ws,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await;
    recv_until(&mut ws, "helloAck", |m| {
        matches!(m, ServerMessage::HelloAck { .. })
    })
    .await;
    send(
        &mut ws,
        &ClientMessage::Register {
            username: user.clone(),
            password: "pw".into(),
        },
    )
    .await;
    recv_until(&mut ws, "loginResult", |m| {
        matches!(m, ServerMessage::LoginResult { .. })
    })
    .await;
    send(
        &mut ws,
        &ClientMessage::CreateCharacter {
            name: format!("法_{}", std::process::id()),
            class: protocol::CharacterClass::Mage,
            gender: "male".into(),
        },
    )
    .await;
    let cid = match recv_until(&mut ws, "characterCreated", |m| {
        matches!(m, ServerMessage::CharacterCreated { .. })
    })
    .await
    {
        ServerMessage::CharacterCreated { character_id, .. } => character_id,
        _ => unreachable!(),
    };
    send(
        &mut ws,
        &ClientMessage::SelectCharacter {
            character_id: cid.clone(),
        },
    )
    .await;
    let mut pos = match recv_until(&mut ws, "zoneChanged", |m| {
        matches!(m, ServerMessage::ZoneChanged { .. })
    })
    .await
    {
        ServerMessage::ZoneChanged { position, .. } => position,
        _ => unreachable!(),
    };

    // 技能表
    let skills = match recv_until(&mut ws, "skillList", |m| {
        matches!(m, ServerMessage::SkillList { .. })
    })
    .await
    {
        ServerMessage::SkillList { skills } => skills,
        _ => unreachable!(),
    };
    assert_eq!(skills.len(), 3, "法师应有 3 技能");
    assert_eq!(skills[0].id, "huoqiu");
    println!(
        "[1/4] 技能表 ok: {}",
        skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("/")
    );

    // 接近 → 施放; 被稻草人围殴致死复活时重新接近 (法师 Lv1 很脆)
    let mut got_fx = false;
    let mut got_dmg = None;
    let mut got_mp = None;
    let mut mon_id = String::new();
    'outer: for round in 0..4 {
        // ── 接近阶段: 锁定活稻草人并走到 5.5 格内 ──
        let mut target: Option<(String, Position)> = None;
        for _ in 0..250 {
            let d = (342.0 - pos.x, 157.0 - pos.y);
            let len = (d.0 * d.0 + d.1 * d.1).sqrt().max(1e-6);
            send(
                &mut ws,
                &ClientMessage::Move {
                    direction: Position {
                        x: d.0 / len * 0.12,
                        y: d.1 / len * 0.12,
                    },
                },
            )
            .await;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
            while let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout_at(deadline, ws.next()).await
            {
                if let Ok(ServerMessage::StateUpdate { entities, .. }) =
                    protocol::decode_server(&text)
                {
                    if let Some(p) = entities
                        .iter()
                        .find(|e| e.id == cid)
                        .and_then(|e| e.position)
                    {
                        pos = p;
                    }
                    target = entities
                        .iter()
                        .filter(|e| {
                            e.id.starts_with("mon_5_scarecrow")
                                && e.removed != Some(true)
                                && e.hp.unwrap_or(1) > 0
                        })
                        .filter_map(|e| e.position.map(|p| (e.id.clone(), p)))
                        .min_by(|a, b| {
                            let da = (a.1.x - pos.x).powi(2) + (a.1.y - pos.y).powi(2);
                            let db = (b.1.x - pos.x).powi(2) + (b.1.y - pos.y).powi(2);
                            da.total_cmp(&db)
                        });
                }
            }
            if let Some((_, mp)) = &target {
                if ((mp.x - pos.x).powi(2) + (mp.y - pos.y).powi(2)).sqrt() < 5.5 {
                    break;
                }
            }
        }
        let Some((tid, _)) = target else {
            continue 'outer;
        };
        mon_id = tid;
        println!("[2/4] 进入射程 (第 {} 轮), 目标 {mon_id}", round + 1);

        // ── 施放阶段: 重试直到集齐 特效/伤害/扣蓝; 死亡复活则重新接近 ──
        for _ in 0..10 {
            send(
                &mut ws,
                &ClientMessage::UseSkill {
                    skill_id: "huoqiu".into(),
                    target_id: Some(mon_id.clone()),
                    position: None,
                },
            )
            .await;
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(1700);
            while let Ok(Some(Ok(Message::Text(text)))) =
                tokio::time::timeout_at(deadline, ws.next()).await
            {
                match protocol::decode_server(&text) {
                    Ok(ServerMessage::SkillEffect {
                        skill_id, targets, ..
                    }) if skill_id == "huoqiu" => {
                        assert!(targets.contains(&mon_id));
                        got_fx = true;
                    }
                    Ok(ServerMessage::DamageNumber {
                        target_id, amount, ..
                    }) if target_id == mon_id => {
                        got_dmg = Some(amount);
                    }
                    Ok(ServerMessage::PlayerStatus { mp, .. })
                        if got_dmg.is_some() && got_mp.is_none() =>
                    {
                        got_mp = Some(mp);
                    }
                    Ok(ServerMessage::ZoneChanged { position, .. }) => {
                        // 被打死复活 → 重新接近
                        pos = position;
                        println!("    (阵亡复活, 重新接近)");
                        continue 'outer;
                    }
                    Ok(ServerMessage::StateUpdate { entities, .. }) => {
                        if let Some(p) = entities
                            .iter()
                            .find(|e| e.id == cid)
                            .and_then(|e| e.position)
                        {
                            pos = p;
                        }
                        // 追近保持射程
                        if let Some(mp2) = entities
                            .iter()
                            .find(|e| e.id == mon_id)
                            .and_then(|e| e.position)
                        {
                            let d = (mp2.x - pos.x, mp2.y - pos.y);
                            let len = (d.0 * d.0 + d.1 * d.1).sqrt();
                            if len > 5.5 {
                                send(
                                    &mut ws,
                                    &ClientMessage::Move {
                                        direction: Position {
                                            x: d.0 / len * 0.12,
                                            y: d.1 / len * 0.12,
                                        },
                                    },
                                )
                                .await;
                            }
                        }
                    }
                    _ => {}
                }
                if got_fx && got_dmg.is_some() && got_mp.is_some() {
                    break 'outer;
                }
            }
        }
    }
    assert!(got_fx, "未收到 SkillEffect");
    assert_eq!(got_dmg, Some(12), "火球伤害应为 12 (6×2.0)");
    let mp = got_mp.expect("未收到 PlayerStatus");
    assert!(mp < 38, "应已扣蓝: {mp}");
    println!("[3/4] 火球命中 ok: 伤害 12, mp→{mp}, 特效已广播");

    // 冷却内立刻重放 → 应被拒 (无第二次火球伤害)
    send(
        &mut ws,
        &ClientMessage::UseSkill {
            skill_id: "huoqiu".into(),
            target_id: Some(mon_id.clone()),
            position: None,
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(600);
    let mut second = false;
    while let Ok(Some(Ok(Message::Text(text)))) = tokio::time::timeout_at(deadline, ws.next()).await
    {
        if let Ok(ServerMessage::DamageNumber { target_id, .. }) = protocol::decode_server(&text) {
            if target_id == mon_id {
                second = true;
            }
        }
    }
    assert!(!second, "冷却期间不应二次结算");
    println!("[4/4] 冷却拒绝 ok");
    println!("技能冒烟全过 ✓");
}
