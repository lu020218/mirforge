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
                if let Some(mut item) = make_item(&d.item) {
                    self.roll_rare(&mut item);
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
    /// 掉落滚极品: 按模板概率命中后, 在非零属性上随机分配 1..=上限 点
    pub(super) fn roll_rare(&mut self, item: &mut protocol::ItemInfo) {
        let Some(def) = item_def(&item.template) else {
            return;
        };
        if def.rare_chance <= 0.0 || self.rand01() >= def.rare_chance {
            return;
        }
        // 可加点的属性槽 (0攻 1魔 2道 3防 4HP), 只挑基础非零的
        let mut slots = Vec::new();
        for (i, v) in [item.attack, item.magic, item.spirit, item.defense, item.hp]
            .iter()
            .enumerate()
        {
            if *v > 0 {
                slots.push(i);
            }
        }
        if slots.is_empty() {
            return;
        }
        let points = 1 + (self.rand01() * def.rare_max.max(1) as f64) as i32;
        for _ in 0..points.min(def.rare_max.max(1)) {
            let pick = slots[(self.rand01() * slots.len() as f64) as usize % slots.len()];
            match pick {
                0 => item.attack += 1,
                1 => item.magic += 1,
                2 => item.spirit += 1,
                3 => item.defense += 1,
                _ => item.hp += 1,
            }
            item.bonus += 1;
        }
    }

    /// 武器损耗: 每次出手 1/8 概率 -1; 归零通知损坏 (属性即刻失效)
    pub(super) async fn wear_weapon(&mut self, char_id: &str) {
        if self.rand01() >= 0.125 {
            return;
        }
        let mut broke = None;
        {
            let Some(p) = self.players.get_mut(char_id) else {
                return;
            };
            let Some(w) = p.equipment.get_mut("weapon") else {
                return;
            };
            if w.max_dur <= 0 || w.dur <= 0 {
                return;
            }
            w.dur -= 1;
            if w.dur == 0 {
                broke = Some(w.name.clone());
            }
            p.recalc();
        }
        self.persist_items_gold(char_id).await;
        self.send_inventory(char_id).await;
        if let Some(name) = broke {
            let conn = match self.players.get(char_id) {
                Some(p) => p.conn_id.clone(),
                None => return,
            };
            self.notify(&conn, &format!("你的 {name} 已损坏, 找修理商修理"))
                .await;
        }
    }

    /// 防具损耗: 被击中 1/10 概率随机一件非武器装备 -1
    pub(super) async fn wear_armor(&mut self, char_id: &str) {
        if self.rand01() >= 0.1 {
            return;
        }
        let roll = self.rand01();
        let mut broke = None;
        {
            let Some(p) = self.players.get_mut(char_id) else {
                return;
            };
            let mut keys: Vec<String> = p
                .equipment
                .iter()
                .filter(|(k, i)| k.as_str() != "weapon" && i.max_dur > 0 && i.dur > 0)
                .map(|(k, _)| k.clone())
                .collect();
            if keys.is_empty() {
                return;
            }
            keys.sort(); // HashMap 遍历序不稳定, 排序后取随机才可复现
            let k = keys[(roll * keys.len() as f64) as usize % keys.len()].clone();
            if let Some(it) = p.equipment.get_mut(&k) {
                it.dur -= 1;
                if it.dur == 0 {
                    broke = Some(it.name.clone());
                }
            }
            p.recalc();
        }
        self.persist_items_gold(char_id).await;
        self.send_inventory(char_id).await;
        if let Some(name) = broke {
            let conn = match self.players.get(char_id) {
                Some(p) => p.conn_id.clone(),
                None => return,
            };
            self.notify(&conn, &format!("你的 {name} 已损坏, 找修理商修理"))
                .await;
        }
    }

    /// 一键修理身上全部装备: 费用 = Σ 损耗点 × max(1, 单价/耐久上限)
    pub(super) async fn handle_repair(&mut self, conn_id: &str, npc_id: &str) {
        if self.npc_in_reach(conn_id, npc_id).is_none() {
            return self.notify(conn_id, "离修理商太远了").await;
        }
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let cost: u64 = {
            let Some(p) = self.players.get(&char_id) else {
                return;
            };
            p.equipment
                .values()
                .filter(|i| i.max_dur > 0 && i.dur < i.max_dur)
                .map(|i| {
                    let price = item_def(&i.template).map(|d| d.price).unwrap_or(0) as u64;
                    let per = (price / i.max_dur.max(1) as u64).max(1);
                    (i.max_dur - i.dur) as u64 * per
                })
                .sum()
        };
        if cost == 0 {
            return self.notify(conn_id, "装备完好, 无需修理").await;
        }
        let paid = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.gold < cost {
                false
            } else {
                p.gold -= cost;
                for it in p.equipment.values_mut() {
                    if it.max_dur > 0 {
                        it.dur = it.max_dur;
                    }
                }
                p.recalc();
                true
            }
        };
        if !paid {
            return self
                .notify(conn_id, &format!("修理需要 {cost} 金币, 金币不足"))
                .await;
        }
        self.persist_items_gold(&char_id).await;
        self.send_inventory(&char_id).await;
        let gold = self.players.get(&char_id).map(|p| p.gold).unwrap_or(0);
        self.send_gold(conn_id, gold).await;
        self.notify(conn_id, &format!("修理完成, 花费 {cost} 金币"))
            .await;
    }

    /// 存物品进仓库: 需在该 NPC 交谈距离内
    pub(super) async fn handle_store_item(&mut self, conn_id: &str, npc_id: &str, item_id: &str) {
        if self.npc_in_reach(conn_id, npc_id).is_none() {
            return self.notify(conn_id, "离仓库管理员太远了").await;
        }
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let moved = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.storage.len() >= STORAGE_CAP {
                false
            } else if let Some(pos) = p.inventory.iter().position(|i| i.id == item_id) {
                let item = p.inventory.remove(pos);
                p.storage.push(item);
                true
            } else {
                return;
            }
        };
        if !moved {
            return self.notify(conn_id, "仓库已满").await;
        }
        self.persist_storage(&char_id).await;
        self.send_inventory(&char_id).await;
        self.send_storage(conn_id, &char_id, npc_id).await;
    }

    /// 从仓库取物品
    pub(super) async fn handle_storage_take(&mut self, conn_id: &str, npc_id: &str, item_id: &str) {
        if self.npc_in_reach(conn_id, npc_id).is_none() {
            return self.notify(conn_id, "离仓库管理员太远了").await;
        }
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let moved = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.inventory.len() >= MAX_INVENTORY {
                false
            } else if let Some(pos) = p.storage.iter().position(|i| i.id == item_id) {
                let item = p.storage.remove(pos);
                p.inventory.push(item);
                true
            } else {
                return;
            }
        };
        if !moved {
            return self.notify(conn_id, "背包已满").await;
        }
        self.persist_storage(&char_id).await;
        self.send_inventory(&char_id).await;
        self.send_storage(conn_id, &char_id, npc_id).await;
    }

    async fn persist_storage(&self, char_id: &str) {
        if let Some(p) = self.players.get(char_id) {
            let _ = self.db.save_storage(char_id, &p.storage).await;
            let _ = self
                .db
                .save_items(char_id, &p.inventory, &p.equipment)
                .await;
        }
    }

    pub(super) async fn send_storage(&self, conn_id: &str, char_id: &str, npc_id: &str) {
        let Some(p) = self.players.get(char_id) else {
            return;
        };
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::StorageState {
                npc_id: npc_id.to_string(),
                items: p.storage.clone(),
                cap: STORAGE_CAP as u32,
            },
        )
        .await;
    }

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
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
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
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
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
