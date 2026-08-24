//! 网络线程：阻塞 tungstenite + 非阻塞轮询，channel 对接 Bevy（游戏侧零 async）。
//! 一条线程服务一条连接；断开发 Disconnected 后退出，重连由游戏侧重新 connect。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TryRecvError};

use protocol::{ClientMessage, ServerMessage};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WsError, Message};

/// 网络线程 → 游戏
pub enum NetEvent {
    Connected,
    Msg(ServerMessage),
    Disconnected,
}

pub struct NetClient {
    pub tx: Sender<ClientMessage>,
    pub rx: Receiver<NetEvent>,
}

/// 发起连接（线程内完成握手；失败也经 Disconnected 通知）
pub fn connect(url: String) -> NetClient {
    let (tx_out, rx_out) = crossbeam_channel::unbounded::<ClientMessage>();
    let (tx_in, rx_in) = crossbeam_channel::unbounded::<NetEvent>();
    std::thread::spawn(move || run(url, rx_out, tx_in));
    NetClient {
        tx: tx_out,
        rx: rx_in,
    }
}

fn run(url: String, rx_out: Receiver<ClientMessage>, tx_in: Sender<NetEvent>) {
    let mut socket = match tungstenite::connect(&url) {
        Ok((s, _)) => s,
        Err(e) => {
            eprintln!("连接 {url} 失败: {e}");
            let _ = tx_in.send(NetEvent::Disconnected);
            return;
        }
    };
    if let MaybeTlsStream::Plain(tcp) = socket.get_mut() {
        let _ = tcp.set_nonblocking(true);
    }
    let _ = tx_in.send(NetEvent::Connected);

    let mut outbox: VecDeque<Message> = VecDeque::new();
    let mut last_beat = Instant::now();
    loop {
        // 入站
        loop {
            match socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(m) = protocol::decode_server(&text) {
                        if tx_in.send(NetEvent::Msg(m)).is_err() {
                            return; // 游戏侧已丢弃本客户端
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = tx_in.send(NetEvent::Disconnected);
                    return;
                }
                Ok(_) => {}
                Err(WsError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    let _ = tx_in.send(NetEvent::Disconnected);
                    return;
                }
            }
        }
        // 出站排队
        loop {
            match rx_out.try_recv() {
                Ok(m) => {
                    if let Ok(json) = protocol::encode(&m) {
                        outbox.push_back(Message::Text(json));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // 心跳
        if last_beat.elapsed() > Duration::from_secs(5) {
            last_beat = Instant::now();
            if let Ok(json) = protocol::encode(&ClientMessage::Heartbeat) {
                outbox.push_back(Message::Text(json));
            }
        }
        // 出站发送 (WouldBlock 时留队下轮)
        while let Some(m) = outbox.front() {
            match socket.send(m.clone()) {
                Ok(()) => {
                    outbox.pop_front();
                }
                Err(WsError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    let _ = tx_in.send(NetEvent::Disconnected);
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
