//! 任务冒烟：接取猎鸡 → 杀 3 只鸡计数 → 交付得经验 → 前置解锁猎鹿。
//! 用法: 先起服务器, 再 `cargo run -p mirforge-server --example quests`

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
    let user = format!("qst_{}", std::process::id());

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
            name: format!("任_{}", std::process::id()),
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

    // 初始任务态: 只有猎鸡可接 (猎鹿被前置锁住)
    let quests = match recv_until(&mut ws, "questState", |m| {
        matches!(m, ServerMessage::QuestState { .. })
    })
    .await
    {
        ServerMessage::QuestState { quests } => quests,
        _ => unreachable!(),
    };
    assert_eq!(quests.len(), 1, "初始应只有 1 个可接任务");
    assert_eq!(quests[0].id, "hunt_chicken");
    println!("[1/5] 初始任务态 ok: 仅 {} 可接", quests[0].name);

    // 接取
    send(
        &mut ws,
        &ClientMessage::AcceptQuest {
            quest_id: "hunt_chicken".into(),
        },
    )
    .await;
    recv_until(&mut ws, "接取后 questState", |m| {
        matches!(m, ServerMessage::QuestState { quests }
            if quests.iter().any(|q| q.id == "hunt_chicken" && q.state == "active"))
    })
    .await;
    println!("[2/5] 接取 ok");

    // 杀 3 只鸡 (被动怪, 在 (333,148) 附近游荡)
    let mut killed = 0u32;
    'hunt: for _round in 0..400 {
        // 找最近的活鸡
        let mut chicken: Option<(String, Position)> = None;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(60);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(deadline, ws.next()).await
        {
            if let Ok(m) = protocol::decode_server(&text) {
                match m {
                    ServerMessage::StateUpdate { entities, .. } => {
                        if let Some(p) = entities
                            .iter()
                            .find(|e| e.id == cid)
                            .and_then(|e| e.position)
                        {
                            pos = p;
                        }
                        chicken = entities
                            .iter()
                            .filter(|e| {
                                e.id.starts_with("mon_3_chicken")
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
                    ServerMessage::QuestState { quests } => {
                        if let Some(q) = quests.iter().find(|q| q.id == "hunt_chicken") {
                            killed = q.objectives[0].current;
                            if killed >= 3 {
                                break 'hunt;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let Some((chick_id, cp)) = chicken else {
            continue;
        };
        let d = (cp.x - pos.x, cp.y - pos.y);
        let len = (d.0 * d.0 + d.1 * d.1).sqrt();
        if len > 2.0 {
            send(
                &mut ws,
                &ClientMessage::Move {
                    direction: Position {
                        x: d.0 / len * 0.15,
                        y: d.1 / len * 0.15,
                    },
                },
            )
            .await;
        } else {
            send(
                &mut ws,
                &ClientMessage::Attack {
                    target_id: chick_id,
                    skill_id: "basic".into(),
                },
            )
            .await;
            tokio::time::sleep(std::time::Duration::from_millis(620)).await;
        }
    }
    assert!(killed >= 3, "猎鸡计数不足: {killed}");
    println!("[3/5] 猎鸡 3/3 ok");

    // 交付 → 任务完成通知 + 经验 50 + 猎鹿解锁
    send(
        &mut ws,
        &ClientMessage::CompleteQuest {
            quest_id: "hunt_chicken".into(),
        },
    )
    .await;
    recv_until(&mut ws, "任务完成通知", |m| {
        matches!(m, ServerMessage::Notification { notification_type, .. } if notification_type == "quest")
    })
    .await;
    println!("[4/5] 交付 ok: 任务完成通知");
    let quests = match recv_until(&mut ws, "解锁后 questState", |m| {
        matches!(m, ServerMessage::QuestState { quests }
            if quests.iter().any(|q| q.id == "hunt_deer"))
    })
    .await
    {
        ServerMessage::QuestState { quests } => quests,
        _ => unreachable!(),
    };
    let chicken_q = quests.iter().find(|q| q.id == "hunt_chicken").unwrap();
    let deer_q = quests.iter().find(|q| q.id == "hunt_deer").unwrap();
    assert_eq!(chicken_q.state, "completed");
    assert_eq!(deer_q.state, "available");
    println!("[5/5] 前置链 ok: 猎鸡已完成, 猎鹿解锁");
    println!("任务冒烟全过 ✓");
}
