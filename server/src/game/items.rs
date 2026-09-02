//! 物品与地面掉落: 掷落/拾取/穿脱(自 game.rs 机械拆出,行为不变)。
use super::*;

impl Game {
    /// 击杀掷落: 命中的物品直接入包 (经典拾取交互后续再做)
    pub(super) async fn roll_drops(&mut self, _char_id: &str, mon_id: &str) {
        let Some(drops) = self
            .monsters
            .iter()
            .find(|m| m.id == mon_id)
            .map(|m| m.drops.clone())
        else {
            return;
        };
        let mut gained = Vec::new();
        for d in drops {
            if self.rand01() < d.chance {
                if let Some(item) = make_item(&d.item) {
                    gained.push(item);
                }
            }
        }
        if gained.is_empty() {
            return;
        }
        // 原版语义: 掉落物落地, 玩家点击拾取 (背包满也不丢失)
        let Some((zone, mx, my)) = self
            .monsters
            .iter()
            .find(|m| m.id == mon_id)
            .map(|m| (m.zone.clone(), m.x, m.y))
        else {
            return;
        };
        for (i, item) in gained.into_iter().enumerate() {
            let ang = i as f64 * 2.4;
            let (dx, dy) = (ang.cos() * 0.4 * i as f64, ang.sin() * 0.4 * i as f64);
            self.spawn_ground(&zone, item, mx + dx, my + dy);
        }
        self.broadcast_ground(&zone).await;
    }

    pub(super) fn spawn_ground(&mut self, zone: &str, item: protocol::ItemInfo, x: f64, y: f64) {
        let id = format!("drop_{}", self.next_drop_id);
        self.next_drop_id += 1;
        self.ground.push(GroundItem {
            id,
            zone: zone.to_string(),
            item,
            x,
            y,
            expire: Instant::now() + GROUND_ITEM_TTL,
        });
    }

    /// 区域地面物品全量快照 → 该区在线玩家
    pub(super) async fn broadcast_ground(&self, zone: &str) {
        let items: Vec<protocol::GroundItemInfo> = self
            .ground
            .iter()
            .filter(|g| g.zone == zone)
            .map(|g| protocol::GroundItemInfo {
                id: g.id.clone(),
                name: g.item.name.clone(),
                image: g.item.image,
                x: g.x,
                y: g.y,
            })
            .collect();
        let conns = self.zone_conns(zone);
        broadcast_to(&self.sessions, &conns, ServerMessage::GroundItems { items }).await;
    }

    /// 丢弃: 背包移除 → 脚下落地
    pub(super) async fn handle_drop_item(&mut self, conn_id: &str, item_id: &str) {
        let Some((char_id, zone, x, y, idx)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id && p.connected)
            .and_then(|(id, p)| {
                let idx = p.inventory.iter().position(|i| i.id == item_id)?;
                Some((id.clone(), p.zone.clone(), p.x, p.y, idx))
            })
        else {
            return;
        };
        let item = self
            .players
            .get_mut(&char_id)
            .map(|p| p.inventory.remove(idx));
        if let Some(item) = item {
            self.spawn_ground(&zone, item, x, y);
            self.send_inventory(&char_id).await;
            self.broadcast_ground(&zone).await;
        }
    }

    /// 拾取: 距离与容量校验 → 入包
    pub(super) async fn handle_pickup_item(&mut self, conn_id: &str, drop_id: &str) {
        let Some((char_id, conn, zone, px, py, inv_len)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id && p.connected)
            .map(|(id, p)| {
                (
                    id.clone(),
                    p.conn_id.clone(),
                    p.zone.clone(),
                    p.x,
                    p.y,
                    p.inventory.len(),
                )
            })
        else {
            return;
        };
        let Some(gi) = self
            .ground
            .iter()
            .position(|g| g.id == drop_id && g.zone == zone)
        else {
            return;
        };
        let g = &self.ground[gi];
        if ((g.x - px).powi(2) + (g.y - py).powi(2)).sqrt() > 2.0 {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: "距离太远, 无法拾取".into(),
                    notification_type: "warn".into(),
                },
            )
            .await;
            return;
        }
        if inv_len >= MAX_INVENTORY {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: "背包已满".into(),
                    notification_type: "warn".into(),
                },
            )
            .await;
            return;
        }
        let g = self.ground.remove(gi);
        let name = g.item.name.clone();
        if let Some(p) = self.players.get_mut(&char_id) {
            p.inventory.push(g.item);
        }
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("获得物品: {name}"),
                notification_type: "loot".into(),
            },
        )
        .await;
        self.send_inventory(&char_id).await;
        self.broadcast_ground(&zone).await;
    }

    /// 推送背包与装备
    pub(super) async fn send_inventory(&self, char_id: &str) {
        let Some(p) = self.players.get(char_id) else {
            return;
        };
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::InventoryState {
                inventory: p.inventory.clone(),
                equipment: p.equipment.clone(),
            },
        )
        .await;
    }

    /// 穿装: 槽位由物品模板决定, 原槽装备回包
    pub(super) async fn handle_equip(&mut self, conn_id: &str, item_id: &str) {
        let Some(char_id) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            let Some(idx) = p.inventory.iter().position(|i| i.id == item_id) else {
                return;
            };
            let item = p.inventory.remove(idx);
            if let Some(old) = p.equipment.insert(item.slot.clone(), item) {
                p.inventory.push(old);
            }
            p.recalc();
        }
        self.send_inventory(&char_id).await;
        self.send_player_status(&char_id).await;
    }

    /// 卸装回包
    pub(super) async fn handle_unequip(&mut self, conn_id: &str, slot: &str) {
        let Some(char_id) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            let Some(item) = p.equipment.remove(slot) else {
                return;
            };
            p.inventory.push(item);
            p.recalc();
        }
        self.send_inventory(&char_id).await;
        self.send_player_status(&char_id).await;
    }
}
