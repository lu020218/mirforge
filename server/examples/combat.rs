//! 战斗冒烟：进图 → 接近稻草人 → 连续普攻 → 断言 伤害广播/击杀/经验/怪物移除。
//! 用法: 先起服务器, 再 `cargo run -p mirforge-server --example combat`

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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
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

/// 短暂收包并返回目标怪的最新位置与血量
fn scan_monster(m: &ServerMessage, prefix: &str) -> Option<(String, Position, i32)> {
    if let ServerMessage::StateUpdate { entities, .. } = m {
        entities
            .iter()
            .find(|e| e.id.starts_with(prefix) && e.removed != Some(true) && e.hp.unwrap_or(1) > 0)
            .and_then(|e| Some((e.id.clone(), e.position?, e.hp.unwrap_or(0))))
    } else {
        None
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("MIRFORGE_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".into());
    let url = format!("ws://{addr}");
    let user = format!("cbt_{}", std::process::id());

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
            security_question: None,
            security_answer: None,
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
            name: format!("战_{}", std::process::id()),
            class: protocol::CharacterClass::Warrior,
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
    println!("[1/4] 进图 @({:.1},{:.1})", pos.x, pos.y);

    // ── 走向稻草人区并锁定一只 (它有仇恨会迎面走来) ──
    let mut target: Option<(String, Position, i32)> = None;
    for _ in 0..200 {
        // 朝稻草人刷新点走一步
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
        pos.x += d.0 / len * 0.12;
        pos.y += d.1 / len * 0.12;
        // 收包 50ms, 更新怪位置
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(deadline, ws.next()).await
        {
            if let Ok(m) = protocol::decode_server(&text) {
                if let Some(t) = scan_monster(&m, "mon_5_scarecrow") {
                    target = Some(t);
                }
            }
        }
        if let Some((_, mp, _)) = &target {
            let d = ((mp.x - pos.x).powi(2) + (mp.y - pos.y).powi(2)).sqrt();
            if d < 2.0 {
                break;
            }
        }
    }
    let (mon_id, _, hp0) = target.expect("没遇到稻草人");
    println!("[2/4] 锁定 {mon_id} (hp {hp0}), 开打");

    // ── 连续普攻直到死亡 ──
    let mut total_dmg = 0;
    let mut exp_note = None;
    let mut removed = false;
    for _ in 0..30 {
        send(
            &mut ws,
            &ClientMessage::Attack {
                target_id: mon_id.clone(),
                skill_id: "basic".into(),
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(700);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(deadline, ws.next()).await
        {
            match protocol::decode_server(&text) {
                Ok(ServerMessage::DamageNumber {
                    target_id, amount, ..
                }) if target_id == mon_id => {
                    total_dmg += amount;
                }
                Ok(ServerMessage::Notification {
                    message,
                    notification_type,
                }) if notification_type == "exp" => {
                    exp_note = Some(message);
                }
                Ok(ServerMessage::StateUpdate { entities, .. }) => {
                    if entities
                        .iter()
                        .any(|e| e.id == mon_id && e.removed == Some(true))
                    {
                        removed = true;
                    }
                    // 怪追打时跟着贴身 (保持射程)
                    if let Some(e) = entities.iter().find(|e| e.id == mon_id) {
                        if let Some(mp) = e.position {
                            let d = (mp.x - pos.x, mp.y - pos.y);
                            let len = (d.0 * d.0 + d.1 * d.1).sqrt();
                            if len > 1.8 {
                                send(
                                    &mut ws,
                                    &ClientMessage::Move {
                                        direction: Position {
                                            x: d.0 / len * 0.1,
                                            y: d.1 / len * 0.1,
                                        },
                                    },
                                )
                                .await;
                                pos.x += d.0 / len * 0.1;
                                pos.y += d.1 / len * 0.1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if removed {
            break;
        }
    }
    assert!(total_dmg >= 90, "累计伤害不足杀怪: {total_dmg}");
    println!("[3/4] 击杀确认: 累计伤害 {total_dmg} (怪 hp {hp0}), removed={removed}");
    assert!(removed, "怪物死后未广播 removed");
    let exp = exp_note.expect("未收到经验通知");
    println!("[4/4] 经验入账: {exp}");
    println!("战斗冒烟全过 ✓");
}
