//! 任务链: 接取/推进/交付/放弃(自 game.rs 机械拆出,行为不变)。
use super::*;

impl Game {
    /// 击杀怪物 → 推进进行中任务的对应目标
    pub(super) async fn progress_quests(&mut self, char_id: &str, template: &str) {
        let Some(p) = self.players.get_mut(char_id) else {
            return;
        };
        let mut changed = false;
        for (qid, prog) in p.quests.iter_mut() {
            if prog.state != 1 {
                continue;
            }
            let Some(def) = quest_def(qid) else { continue };
            for (i, (target, required)) in def.objectives.iter().enumerate() {
                if *target == template && prog.counts[i] < *required {
                    prog.counts[i] += 1;
                    changed = true;
                }
            }
        }
        if changed {
            self.send_quests(char_id).await;
        }
    }

    /// 推送任务面板全量态 (可接/进行中/已完成)
    pub(super) async fn send_quests(&self, char_id: &str) {
        let Some(p) = self.players.get(char_id) else {
            return;
        };
        let quests = data()
            .quests
            .iter()
            .filter_map(|def| {
                let prog = p.quests.get(&def.id);
                let state = match prog {
                    Some(q) if q.state == 2 => "completed",
                    Some(_) => "active",
                    None => {
                        // 前置完成才可接
                        let ok = def
                            .prereq
                            .as_deref()
                            .is_none_or(|pr| p.quests.get(pr).is_some_and(|q| q.state == 2));
                        if !ok {
                            return None;
                        }
                        "available"
                    }
                };
                Some(protocol::QuestInfo {
                    id: def.id.to_string(),
                    name: def.name.to_string(),
                    state: state.to_string(),
                    objectives: def
                        .objectives
                        .iter()
                        .enumerate()
                        .map(|(i, (target, required))| protocol::QuestObjectiveInfo {
                            target_id: target.to_string(),
                            current: prog.map(|q| q.counts[i]).unwrap_or(0).min(*required),
                            required: *required,
                        })
                        .collect(),
                    exp_reward: def.exp_reward,
                    gold_reward: def.gold_reward,
                    // 名称/图标在服务端解析好, 客户端不必再查物品表
                    item_rewards: def
                        .rewards
                        .iter()
                        .map(|rw| {
                            let d = item_def(&rw.item);
                            protocol::QuestRewardInfo {
                                name: d
                                    .as_ref()
                                    .map(|d| d.name.clone())
                                    .unwrap_or_else(|| rw.item.clone()),
                                count: rw.count,
                                image: d.map(|d| d.image).unwrap_or(0),
                            }
                        })
                        .collect(),
                })
            })
            .collect();
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::QuestState { quests },
        )
        .await;
    }

    pub(super) async fn handle_accept_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(def) = quest_def(quest_id) else {
            return;
        };
        {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.quests.contains_key(quest_id) {
                return;
            }
            let prereq_ok = def
                .prereq
                .as_deref()
                .is_none_or(|pr| p.quests.get(pr).is_some_and(|q| q.state == 2));
            if !prereq_ok {
                return;
            }
            p.quests.insert(
                quest_id.to_string(),
                QuestProgress {
                    state: 1,
                    counts: vec![0; def.objectives.len()],
                },
            );
        }
        self.send_quests(&char_id).await;
    }

    pub(super) async fn handle_complete_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(def) = quest_def(quest_id) else {
            return;
        };
        {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            let Some(prog) = p.quests.get_mut(quest_id) else {
                return;
            };
            if prog.state != 1 {
                return;
            }
            let done = def
                .objectives
                .iter()
                .enumerate()
                .all(|(i, (_, required))| prog.counts[i] >= *required);
            if !done {
                return;
            }
        }
        let Some(conn) = self.players.get(&char_id).map(|p| p.conn_id.clone()) else {
            return;
        };
        // 物品奖励先备好并占位检查 —— 位置不够就整单不发, 否则奖励会凭空消失
        let payout: Vec<protocol::ItemInfo> = def
            .rewards
            .iter()
            .flat_map(|rw| (0..rw.count).filter_map(|_| make_item(&rw.item)))
            .collect();
        {
            let Some(p) = self.players.get(&char_id) else {
                return;
            };
            if p.inventory.len() + payout.len() > MAX_INVENTORY {
                self.notify(
                    &conn,
                    &format!("背包空位不足 ({} 件奖励), 整理后再来交任务", payout.len()),
                )
                .await;
                return;
            }
        }
        let gold = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            let Some(q) = p.quests.get_mut(quest_id) else {
                return;
            };
            q.state = 2;
            p.inventory.extend(payout.iter().cloned());
            p.gold = p.gold.saturating_add(def.gold_reward);
            p.gold
        };
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("任务完成: {}", def.name),
                notification_type: "quest".into(),
            },
        )
        .await;
        self.award_exp(&char_id, def.exp_reward).await;
        if def.gold_reward > 0 {
            let _ = self.db.save_gold(&char_id, gold).await;
            self.send_gold(&conn, gold).await;
            self.notify(&conn, &format!("获得金币 {}", def.gold_reward))
                .await;
        }
        if !payout.is_empty() {
            let names: Vec<String> = payout.iter().map(|i| i.name.clone()).collect();
            self.notify(&conn, &format!("获得物品: {}", names.join("、")))
                .await;
            self.send_inventory(&char_id).await;
        }
        self.send_quests(&char_id).await;
    }

    pub(super) async fn handle_abandon_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            match p.quests.get(quest_id) {
                Some(q) if q.state == 1 => {
                    p.quests.remove(quest_id);
                }
                _ => return,
            }
        }
        self.send_quests(&char_id).await;
    }
}
