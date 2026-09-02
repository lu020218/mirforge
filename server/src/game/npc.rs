//! NPC 交互: 对话/商店/传送/落位校验(自 game.rs 机械拆出,行为不变)。
use super::*;

impl Game {
    /// 某区域的启用 NPC (下发客户端)
    pub(super) fn npcs_of_zone(zone: &str) -> Vec<protocol::NpcInfo> {
        data()
            .npcs
            .iter()
            .filter(|n| n.enabled && n.map == zone)
            .map(|n| protocol::NpcInfo {
                id: n.id.clone(),
                name: n.name.clone(),
                x: n.x,
                y: n.y,
                image: n.image,
            })
            .collect()
    }

    /// 向某连接下发其所在区的 NPC 列表
    pub(super) async fn send_npc_list(&self, conn_id: &str, zone: &str) {
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcList {
                npcs: Self::npcs_of_zone(zone),
            },
        )
        .await;
    }

    /// 取某 NPC 的某一页; page=0 表示「第一页」(取页号最小的一页)
    /// 没配对话页时的缺省页
    ///
    /// 商店 NPC 直接带一个进店入口 —— 否则"类型选了商店、货也上架了"却依然
    /// 只能看到一句招呼, 没有任何办法把店打开。
    pub(super) fn default_page(npc: &NpcDef) -> NpcDialogPage {
        let shop = npc.kind == "shop" && !npc.shop.is_empty();
        let mut options = Vec::new();
        if shop {
            options.push(NpcOptionDef {
                label: "我看看货".into(),
                action: "shop".into(),
                arg: String::new(),
            });
        }
        options.push(NpcOptionDef {
            label: "告辞".into(),
            action: "close".into(),
            arg: String::new(),
        });
        NpcDialogPage {
            page: 1,
            text: if shop {
                format!("{}：看看要点什么？", npc.name)
            } else {
                format!("{}：勇士，愿玛法大陆保佑你。", npc.name)
            },
            options,
        }
    }

    /// 取某页; page=0 表示第一页。没配对话页时回缺省页, 让下游只有一条路径
    pub(super) fn npc_page(npc: &NpcDef, page: u32) -> Option<NpcDialogPage> {
        if npc.dialogs.is_empty() {
            return matches!(page, 0 | 1).then(|| Self::default_page(npc));
        }
        if page == 0 {
            npc.dialogs.iter().min_by_key(|d| d.page).cloned()
        } else {
            npc.dialogs.iter().find(|d| d.page == page).cloned()
        }
    }

    /// 下发一页对话; 该页不存在则结束对话
    pub(super) async fn send_npc_page(&self, conn_id: &str, npc: &NpcDef, page: u32) {
        let Some(d) = Self::npc_page(npc, page) else {
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcDialog {
                npc_id: npc.id.clone(),
                name: npc.name.clone(),
                page: d.page,
                text: d.text.clone(),
                options: d
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| protocol::NpcDialogOption {
                        idx: i as u32,
                        label: o.label.clone(),
                    })
                    .collect(),
            },
        )
        .await;
    }

    /// 取玩家同区且在交互距离内的 NPC (越权/隔图对话在此挡掉)
    pub(super) fn npc_in_reach(&self, conn_id: &str, npc_id: &str) -> Option<NpcDef> {
        let p = self
            .players
            .values()
            .find(|p| p.conn_id == conn_id && p.connected)?;
        let d = data();
        let npc = d
            .npcs
            .iter()
            .find(|n| n.enabled && n.id == npc_id && n.map == p.zone)?;
        let dist = ((npc.x - p.x).powi(2) + (npc.y - p.y).powi(2)).sqrt();
        (dist <= sim::NPC_TALK_RANGE).then(|| npc.clone())
    }

    /// 点击 NPC: 发第一页; 没配对话就用缺省招呼
    pub(super) async fn handle_talk_npc(&mut self, conn_id: &str, npc_id: &str) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            return;
        };
        self.send_npc_page(conn_id, &npc, 0).await;
    }

    /// NPC 传送: arg = "地图:x:y"; 落点不可站立时吸附到最近可走格
    pub(super) async fn teleport_by_npc(&mut self, conn_id: &str, arg: &str) {
        let Some((map, x, y)) = parse_teleport(arg) else {
            return;
        };
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(target) = self.zones.get(&map) else {
            return;
        };
        let (tx, ty) = nearest_walkable(&target.walk, x, y);
        {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            info!("NPC 传送: {} {} → {map} ({tx:.1},{ty:.1})", char_id, p.zone);
            p.zone = map;
            p.x = tx;
            p.y = ty;
            p.moving = false;
        }
        self.send_enter_zone_only(conn_id, &char_id, tx, ty).await;
    }

    /// 商店某件货的实际售价 (商店未单独定价则用物品基准价)
    pub(super) fn shop_price(e: &ShopEntry) -> u32 {
        if e.price > 0 {
            e.price
        } else {
            item_def(&e.item).map(|d| d.price).unwrap_or(0)
        }
    }

    /// 打开商店: 把货架下发给客户端
    pub(super) async fn send_npc_shop(&self, conn_id: &str, npc: &NpcDef) {
        let items: Vec<protocol::ShopItemInfo> = npc
            .shop
            .iter()
            .filter(|e| e.stock != 0) // 卖光的不上架
            .filter_map(|e| {
                let d = item_def(&e.item)?;
                Some(protocol::ShopItemInfo {
                    template: d.template,
                    name: d.name,
                    image: d.image,
                    price: Self::shop_price(e),
                    stock: e.stock,
                    attack: d.attack,
                    magic: d.magic,
                    spirit: d.spirit,
                    defense: d.defense,
                    hp: d.hp,
                    slot: d.slot,
                })
            })
            .collect();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcShop {
                npc_id: npc.id.clone(),
                name: npc.name.clone(),
                items,
                sell_rate: SELL_RATE,
            },
        )
        .await;
    }

    /// 某区在线玩家的位置 (刷怪时让开用)
    pub(super) fn player_positions(
        players: &HashMap<String, PlayerState>,
        zone: &str,
    ) -> Vec<(f64, f64)> {
        players
            .values()
            .filter(|p| p.connected && p.zone == zone)
            .map(|p| (p.x, p.y))
            .collect()
    }

    /// 某区里会挡路的实体圆心 (可排除自己)
    ///
    /// NPC 是站着不动的摊主一类, 也该挡; 尸体 (死亡动画/等重生) 不挡。
    /// 怪之间不互相阻挡 —— 刷新点是按半径随机撒的, 互挡会让整窝当场卡死。
    pub(super) fn blockers_for(
        players: &HashMap<String, PlayerState>,
        monsters: &[Monster],
        zone: &str,
        except: Option<String>,
    ) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = players
            .iter()
            .filter(|(id, p)| {
                p.connected && p.zone == zone && except.as_deref() != Some(id.as_str())
            })
            .map(|(_, p)| (p.x, p.y))
            .collect();
        out.extend(
            monsters
                .iter()
                .filter(|m| m.zone == zone && m.alive())
                .map(|m| (m.x, m.y)),
        );
        out.extend(
            data()
                .npcs
                .iter()
                .filter(|n| n.enabled && n.map == zone)
                .map(|n| (n.x, n.y)),
        );
        out
    }

    /// 全服公告 (BOSS 击杀等)
    pub(super) async fn broadcast_all(&self, msg: &str) {
        let conns: Vec<String> = self
            .players
            .values()
            .filter(|p| p.connected)
            .map(|p| p.conn_id.clone())
            .collect();
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::Notification {
                message: msg.into(),
                notification_type: "system".into(),
            },
        )
        .await;
    }

    pub(super) async fn notify(&self, conn_id: &str, msg: &str) {
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::Notification {
                message: msg.into(),
                notification_type: "system".into(),
            },
        )
        .await;
    }

    pub(super) async fn send_gold(&self, conn_id: &str, gold: u64) {
        send_to(&self.sessions, conn_id, ServerMessage::GoldChanged { gold }).await;
    }

    /// 买入一件: 距离/货架/库存/金币/背包位 逐项校验
    pub(super) async fn handle_buy_item(&mut self, conn_id: &str, npc_id: &str, template: &str) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            return;
        };
        let Some(entry) = npc.shop.iter().find(|e| e.item == template) else {
            return; // 不卖这个 —— 伪造的请求, 静默丢弃
        };
        if entry.stock == 0 {
            self.notify(conn_id, "这件货已经卖完了").await;
            return;
        }
        let price = Self::shop_price(entry);
        let Some(item) = make_item(template) else {
            return;
        };
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        // 先判定再动状态: notify 要借 &self, 不能在持有 players 可变借用时调
        enum Deny {
            BagFull,
            NoGold,
        }
        let deny = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.inventory.len() >= MAX_INVENTORY {
                Some(Deny::BagFull)
            } else if p.gold < price as u64 {
                Some(Deny::NoGold)
            } else {
                p.gold -= price as u64;
                p.inventory.push(item);
                None
            }
        };
        match deny {
            Some(Deny::BagFull) => return self.notify(conn_id, "背包已满").await,
            Some(Deny::NoGold) => return self.notify(conn_id, "金币不足").await,
            None => {}
        }
        // 有限库存要扣, 并把新货架推回去
        if entry.stock > 0 {
            let mut d = (*data()).clone();
            if let Some(e) = d
                .npcs
                .iter_mut()
                .find(|n| n.id == npc_id)
                .and_then(|n| n.shop.iter_mut().find(|e| e.item == template))
            {
                e.stock -= 1;
            }
            set_data(d);
        }
        let gold = self.players.get(&char_id).map(|p| p.gold).unwrap_or(0);
        let _ = self.db.save_gold(&char_id, gold).await;
        self.send_gold(conn_id, gold).await;
        self.send_inventory(&char_id).await;
        if let Some(npc) = self.npc_in_reach(conn_id, npc_id) {
            self.send_npc_shop(conn_id, &npc).await; // 库存变了, 刷新货架
        }
    }

    /// 卖出一件: 按售价 × SELL_RATE 回收
    pub(super) async fn handle_sell_item(&mut self, conn_id: &str, npc_id: &str, item_id: &str) {
        if self.npc_in_reach(conn_id, npc_id).is_none() {
            return;
        }
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let sold = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            let Some(idx) = p.inventory.iter().position(|i| i.id == item_id) else {
                return;
            };
            let item = p.inventory.remove(idx);
            let base = item_def(&item.template).map(|d| d.price).unwrap_or(0);
            let paid = ((base as f64) * SELL_RATE).floor() as u64;
            p.gold = p.gold.saturating_add(paid);
            (item.name, paid, p.gold)
        };
        let (name, paid, gold) = sold;
        let _ = self.db.save_gold(&char_id, gold).await;
        self.send_gold(conn_id, gold).await;
        self.send_inventory(&char_id).await;
        self.notify(conn_id, &format!("卖出 {name}, 得 {paid} 金币"))
            .await;
    }

    /// 选项动作分发
    pub(super) async fn handle_npc_option(
        &mut self,
        conn_id: &str,
        npc_id: &str,
        page: u32,
        idx: u32,
    ) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            // 走远了 / NPC 被禁用 — 明确收场, 免得客户端挂着个死对话框
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        let Some(opt) =
            Self::npc_page(&npc, page).and_then(|d| d.options.get(idx as usize).cloned())
        else {
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        match opt.action.as_str() {
            "page" => {
                let next = opt.arg.parse::<u32>().unwrap_or(0);
                self.send_npc_page(conn_id, &npc, next).await;
            }
            "quest_accept" => {
                self.handle_accept_quest(conn_id, &opt.arg).await;
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            }
            "quest_complete" => {
                self.handle_complete_quest(conn_id, &opt.arg).await;
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            }
            "teleport" => {
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
                self.teleport_by_npc(conn_id, &opt.arg).await;
            }
            "shop" => {
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
                self.send_npc_shop(conn_id, &npc).await;
            }
            _ => send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await,
        }
    }

    /// NPC 落位校验: 地图已接入 / 坐标可走 / 传送目标合法
    ///
    /// 与 `GameData::validate` 分开, 因为走格与区域表只在游戏循环里有。
    pub(super) fn check_npc_placement(&self, npcs: &[NpcDef]) -> Vec<String> {
        let mut errs = Vec::new();
        for n in npcs {
            let Some(z) = self.zones.get(&n.map) else {
                errs.push(format!("NPC {} 的地图未接入为区域: {}", n.id, n.map));
                continue;
            };
            if !z.walk.is_walkable_circle(n.x, n.y, BODY_RADIUS) {
                errs.push(format!(
                    "NPC {} 的坐标不可站立 ({:.1},{:.1})",
                    n.id, n.x, n.y
                ));
            }
            for d in &n.dialogs {
                for o in &d.options {
                    if o.action != "teleport" {
                        continue;
                    }
                    match parse_teleport(&o.arg) {
                        Some((map, x, y)) => match self.zones.get(&map) {
                            None => errs.push(format!(
                                "NPC {} 第 {} 页选项「{}」传送到未接入的地图: {map}",
                                n.id, d.page, o.label
                            )),
                            Some(t) => {
                                if !t.walk.is_walkable_circle(x, y, BODY_RADIUS) {
                                    errs.push(format!(
                                        "NPC {} 第 {} 页选项「{}」传送落点不可站立: {map} ({x:.1},{y:.1})",
                                        n.id, d.page, o.label
                                    ));
                                }
                            }
                        },
                        None => errs.push(format!(
                            "NPC {} 第 {} 页选项「{}」传送参数应为 地图:x:y, 收到: {}",
                            n.id, d.page, o.label, o.arg
                        )),
                    }
                }
            }
        }
        errs
    }
}
