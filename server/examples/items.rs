//! 物品冒烟：杀稻草人 (铁剑 100% 掉落) → 入包 → 穿戴 → 伤害提升 → 卸下。
//! 用法: 先起服务器, 再 `cargo run -p mirforge-server --example items`

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
    let user = format!("loot_{}", std::process::id());

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
            name: format!("猎_{}", std::process::id()),
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

    // 接近并杀一只稻草人 (与 combat 冒烟同法)
    let mut target: Option<(String, Position)> = None;
    for _ in 0..200 {
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
            if let Ok(ServerMessage::StateUpdate { entities, .. }) = protocol::decode_server(&text)
            {
                if let Some(p) = entities
                    .iter()
                    .find(|e| e.id == cid)
                    .and_then(|e| e.position)
                {
                    pos = p;
                }
                if let Some(e) = entities.iter().find(|e| {
                    e.id.starts_with("mon_5_scarecrow")
                        && e.removed != Some(true)
                        && e.hp.unwrap_or(1) > 0
                }) {
                    if let Some(mp) = e.position {
                        target = Some((e.id.clone(), mp));
                    }
                }
            }
        }
        if let Some((_, mp)) = &target {
            if ((mp.x - pos.x).powi(2) + (mp.y - pos.y).powi(2)).sqrt() < 2.0 {
                break;
            }
        }
    }
    let (mon_id, _) = target.expect("没找到稻草人");
    println!("[1/5] 锁定 {mon_id}");

    // 裸装伤害应为 6 (Lv1, 无武器); 追进重试直到首击命中
    let mut bare = None;
    'first: for _ in 0..20 {
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
                    bare = Some(amount);
                    break 'first;
                }
                Ok(ServerMessage::StateUpdate { entities, .. }) => {
                    if let Some(p) = entities
                        .iter()
                        .find(|e| e.id == cid)
                        .and_then(|e| e.position)
                    {
                        pos = p;
                    }
                    if let Some(mp2) = entities
                        .iter()
                        .find(|e| e.id == mon_id)
                        .and_then(|e| e.position)
                    {
                        let d = (mp2.x - pos.x, mp2.y - pos.y);
                        let len = (d.0 * d.0 + d.1 * d.1).sqrt();
                        if len > 1.8 {
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
        }
    }
    let bare = bare.expect("裸装首击始终未命中");
    assert_eq!(bare, 6, "裸装伤害应为 6");
    println!("[2/5] 裸装伤害 ok: {bare}");

    // 杀掉它, 等铁剑入包
    let mut sword_id = None;
    'kill: for _ in 0..30 {
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
            if let Ok(ServerMessage::InventoryState { inventory, .. }) =
                protocol::decode_server(&text)
            {
                if let Some(item) = inventory.iter().find(|i| i.template == "iron_sword") {
                    sword_id = Some(item.id.clone());
                    break 'kill;
                }
            }
        }
    }
    let sword_id = sword_id.expect("击杀后未掉落铁剑");
    println!("[3/5] 击杀掉落 ok: 铁剑入包");

    // 穿戴 → 装备表出现武器
    send(
        &mut ws,
        &ClientMessage::Equip {
            item_id: sword_id.clone(),
            slot: "weapon".into(),
        },
    )
    .await;
    match recv_until(&mut ws, "穿戴后 InventoryState", |m| {
        matches!(m, ServerMessage::InventoryState { equipment, .. } if equipment.get("weapon").is_some())
    }).await {
        ServerMessage::InventoryState { equipment, inventory } => {
            assert_eq!(equipment["weapon"].template, "iron_sword");
            assert!(!inventory.iter().any(|i| i.id == sword_id), "穿戴后不应留在背包");
        }
        _ => unreachable!(),
    }
    println!("[4/5] 穿戴 ok: weapon=铁剑");

    // 持剑伤害 = 6 + 6 = 12 (打另一只怪)
    let mut second: Option<String> = None;
    for _ in 0..100 {
        send(
            &mut ws,
            &ClientMessage::Move {
                direction: Position { x: 0.1, y: 0.05 },
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(50);
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout_at(deadline, ws.next()).await
        {
            if let Ok(ServerMessage::StateUpdate { entities, .. }) = protocol::decode_server(&text)
            {
                if let Some(p) = entities
                    .iter()
                    .find(|e| e.id == cid)
                    .and_then(|e| e.position)
                {
                    pos = p;
                }
                if let Some(e) = entities.iter().find(|e| {
                    e.id.starts_with("mon_")
                        && e.id != mon_id
                        && e.removed != Some(true)
                        && e.position.is_some_and(|mp| {
                            ((mp.x - pos.x).powi(2) + (mp.y - pos.y).powi(2)).sqrt() < 2.0
                        })
                }) {
                    second = Some(e.id.clone());
                }
            }
        }
        if second.is_some() {
            break;
        }
    }
    let second = second.expect("没找到第二只怪");
    // 边追边打 (目标可能在游荡)
    let mut armed = None;
    'strike: for _ in 0..20 {
        send(
            &mut ws,
            &ClientMessage::Attack {
                target_id: second.clone(),
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
                }) if target_id == second => {
                    armed = Some(amount);
                    break 'strike;
                }
                Ok(ServerMessage::StateUpdate { entities, .. }) => {
                    if let Some(p) = entities
                        .iter()
                        .find(|e| e.id == cid)
                        .and_then(|e| e.position)
                    {
                        pos = p;
                    }
                    if let Some(mp) = entities
                        .iter()
                        .find(|e| e.id == second)
                        .and_then(|e| e.position)
                    {
                        let d = (mp.x - pos.x, mp.y - pos.y);
                        let len = (d.0 * d.0 + d.1 * d.1).sqrt();
                        if len > 1.5 {
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
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let armed = armed.expect("持剑攻击始终未命中");
    assert_eq!(armed, 12, "持剑伤害应为 12 (6+6)");

    // 卸下 → 回包
    send(
        &mut ws,
        &ClientMessage::Unequip {
            slot: "weapon".into(),
        },
    )
    .await;
    match recv_until(&mut ws, "卸下后 InventoryState", |m| {
        matches!(m, ServerMessage::InventoryState { equipment, .. } if equipment.get("weapon").is_none())
    }).await {
        ServerMessage::InventoryState { inventory, .. } => {
            assert!(inventory.iter().any(|i| i.id == sword_id), "卸下后应回背包");
        }
        _ => unreachable!(),
    }
    println!("[5/5] 持剑伤害 {armed} (6+6) + 卸下回包 ok");
    println!("物品冒烟全过 ✓");
}
