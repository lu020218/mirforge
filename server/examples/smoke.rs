//! 协议级端到端冒烟：hello → 注册 → 建角 → 进图 → 移动 → 断线 → Resume 原位恢复。
//! 用法: 先起服务器, 再 `cargo run -p mirforge-server --example smoke`
//! (MIRFORGE_ADDR 可指非默认地址)

use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMessage, Position, ServerMessage, PROTOCOL_VERSION};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send(ws: &mut Ws, msg: &ClientMessage) {
    ws.send(Message::Text(protocol::encode(msg).unwrap().into()))
        .await
        .unwrap();
}

/// 读消息直到谓词命中 (跳过不相关广播), 3s 超时
async fn recv_until<F: Fn(&ServerMessage) -> bool>(ws: &mut Ws, what: &str, f: F) -> ServerMessage {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
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

/// 等服务器处理完在途移动包后取最终位置 (400ms 沉降, 再读 300ms 窗口取最后一条广播)
async fn settled_pos(ws: &mut Ws, id: &str) -> Position {
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
    let mut last = None;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, ws.next()).await {
        if let Message::Text(text) = msg {
            if let Ok(ServerMessage::StateUpdate { entities, .. }) = protocol::decode_server(&text)
            {
                if let Some(e) = entities.iter().find(|e| e.id == id) {
                    last = e.position;
                }
            }
        }
    }
    last.expect("没收到含本角色的 stateUpdate")
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("MIRFORGE_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".into());
    let url = format!("ws://{addr}");
    let user = format!("smoke_{}", std::process::id());

    // ── 连接 + 协商 + 注册 + 建角 + 进图 ──
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
    println!("[1/6] hello 协商 ok");

    send(
        &mut ws,
        &ClientMessage::Register {
            username: user.clone(),
            password: "pw123".into(),
        },
    )
    .await;
    let token = match recv_until(&mut ws, "sessionToken", |m| {
        matches!(m, ServerMessage::SessionToken { .. })
    })
    .await
    {
        ServerMessage::SessionToken { token } => token,
        _ => unreachable!(),
    };
    println!("[2/6] 注册 + 会话令牌 ok");

    send(
        &mut ws,
        &ClientMessage::CreateCharacter {
            name: format!("冒烟_{}", std::process::id()),
            class: protocol::CharacterClass::Warrior,
            gender: "male".into(),
        },
    )
    .await;
    let character_id = match recv_until(&mut ws, "characterCreated", |m| {
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
            character_id: character_id.clone(),
        },
    )
    .await;
    let spawn = match recv_until(&mut ws, "zoneChanged", |m| {
        matches!(m, ServerMessage::ZoneChanged { .. })
    })
    .await
    {
        ServerMessage::ZoneChanged {
            position,
            zone_name,
            ..
        } => {
            println!(
                "[3/6] 进图 ok: {zone_name} @({:.1},{:.1})",
                position.x, position.y
            );
            position
        }
        _ => unreachable!(),
    };

    // ── 移动: 向右 20 步 × 0.15 格 (合法速度), 应被采纳 ──
    for _ in 0..20 {
        send(
            &mut ws,
            &ClientMessage::Move {
                direction: Position { x: 0.15, y: 0.0 },
            },
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let pos_after = settled_pos(&mut ws, &character_id).await;
    assert!(
        (pos_after.x - (spawn.x + 3.0)).abs() < 0.2,
        "移动未按步长采纳: 期望 ≈{:.2} 实际 {:.2}",
        spawn.x + 3.0,
        pos_after.x
    );
    println!(
        "[4/6] 移动校验 ok: x {:.2} → {:.2} (20×0.15)",
        spawn.x, pos_after.x
    );

    // ── 超速包应被限幅: 一包要求瞬移 50 格 ──
    send(
        &mut ws,
        &ClientMessage::Move {
            direction: Position { x: 50.0, y: 0.0 },
        },
    )
    .await;
    let pos_clamped = settled_pos(&mut ws, &character_id).await;
    assert!(
        pos_clamped.x - pos_after.x < 3.0,
        "超速包未被限幅: {:.2} → {:.2}",
        pos_after.x,
        pos_clamped.x
    );
    println!(
        "[5/6] 超速限幅 ok: 50 格瞬移被压到 +{:.2}",
        pos_clamped.x - pos_after.x
    );

    // ── 断线 → 新连接 Resume → 原位恢复 ──
    drop(ws);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(
        &mut ws2,
        &ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await;
    recv_until(&mut ws2, "helloAck", |m| {
        matches!(m, ServerMessage::HelloAck { .. })
    })
    .await;
    send(
        &mut ws2,
        &ClientMessage::Resume {
            token,
            character_id: character_id.clone(),
        },
    )
    .await;
    match recv_until(&mut ws2, "zoneChanged(resume)", |m| {
        matches!(m, ServerMessage::ZoneChanged { .. })
    })
    .await
    {
        ServerMessage::ZoneChanged { position, .. } => {
            assert!(
                (position.x - pos_clamped.x).abs() < 1e-6
                    && (position.y - pos_clamped.y).abs() < 1e-6,
                "Resume 未原位: 期望 ({:.2},{:.2}) 实际 ({:.2},{:.2})",
                pos_clamped.x,
                pos_clamped.y,
                position.x,
                position.y
            );
            println!(
                "[6/6] Resume 原位恢复 ok @({:.2},{:.2})",
                position.x, position.y
            );
        }
        _ => unreachable!(),
    }
    println!("冒烟全过 ✓");
}
