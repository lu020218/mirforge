//! SQLite 持久化：账号（bcrypt 口令哈希）与角色。
//! 旧仓库明文口令是已记债——新库从第一天就只存哈希。

use protocol::{CharacterClass, CharacterSummary};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// 连接池（配置存储模块共用同一库）
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[derive(Debug, Clone)]
pub struct CharacterRow {
    pub id: String,
    pub name: String,
    pub class: CharacterClass,
    pub gender: String,
    pub level: u32,
    pub exp: u64,
    pub zone: String,
    pub inventory: Vec<protocol::ItemInfo>,
    pub equipment: std::collections::HashMap<String, protocol::ItemInfo>,
    pub quests: std::collections::HashMap<String, crate::game::QuestProgress>,
    pub x: f64,
    pub y: f64,
}

fn class_to_str(c: CharacterClass) -> &'static str {
    match c {
        CharacterClass::Warrior => "Warrior",
        CharacterClass::Mage => "Mage",
        CharacterClass::Taoist => "Taoist",
    }
}

fn class_from_str(s: &str) -> CharacterClass {
    match s {
        "Mage" => CharacterClass::Mage,
        "Taoist" => CharacterClass::Taoist,
        _ => CharacterClass::Warrior,
    }
}

impl Db {
    pub async fn open(path: &str) -> Result<Self, sqlx::Error> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        // :memory: 每连接独立库, 必须锁单连接且不回收 (测试用)
        let pool = if path == ":memory:" {
            SqlitePoolOptions::new()
                .max_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
                .connect_with(opts)
                .await?
        } else {
            // WAL: 写不阻塞读, 断电/崩溃可恢复; busy_timeout 防瞬时锁冲突
            let opts = opts
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_secs(5));
            SqlitePoolOptions::new().connect_with(opts).await?
        };
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS accounts (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS characters (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL REFERENCES accounts(id),
                name TEXT UNIQUE NOT NULL,
                class TEXT NOT NULL,
                gender TEXT NOT NULL DEFAULT 'male',
                level INTEGER NOT NULL DEFAULT 1,
                exp INTEGER NOT NULL DEFAULT 0,
                inventory TEXT NOT NULL DEFAULT '[]',
                equipment TEXT NOT NULL DEFAULT '{}',
                quests TEXT NOT NULL DEFAULT '{}',
                zone TEXT NOT NULL DEFAULT '0.map',
                x REAL NOT NULL,
                y REAL NOT NULL
            )",
        )
        .execute(&pool)
        .await?;
        // 旧开发库升级 (列已存在则忽略)
        let _ = sqlx::query("ALTER TABLE characters ADD COLUMN exp INTEGER NOT NULL DEFAULT 0")
            .execute(&pool)
            .await;
        let _ =
            sqlx::query("ALTER TABLE characters ADD COLUMN inventory TEXT NOT NULL DEFAULT '[]'")
                .execute(&pool)
                .await;
        let _ =
            sqlx::query("ALTER TABLE characters ADD COLUMN equipment TEXT NOT NULL DEFAULT '{}'")
                .execute(&pool)
                .await;
        let _ = sqlx::query("ALTER TABLE characters ADD COLUMN quests TEXT NOT NULL DEFAULT '{}'")
            .execute(&pool)
            .await;
        let _ = sqlx::query("ALTER TABLE characters ADD COLUMN zone TEXT NOT NULL DEFAULT '0.map'")
            .execute(&pool)
            .await;
        Ok(Db { pool })
    }

    /// 注册。用户名占用返回 Ok(None)。
    pub async fn register(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let hash = bcrypt::hash(password, bcrypt::DEFAULT_COST)
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO accounts (id, username, password_hash) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(&hash)
        .execute(&self.pool)
        .await?;
        Ok((r.rows_affected() > 0).then_some(id))
    }

    /// 校验口令。成功返回 account_id。
    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let Some(row) = sqlx::query("SELECT id, password_hash FROM accounts WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let hash: String = row.get("password_hash");
        let ok = bcrypt::verify(password, &hash).unwrap_or(false);
        Ok(ok.then(|| row.get("id")))
    }

    /// 建角。重名返回 Ok(None)。
    pub async fn create_character(
        &self,
        account_id: &str,
        name: &str,
        class: CharacterClass,
        gender: &str,
        zone: &str,
        spawn: (f64, f64),
    ) -> Result<Option<String>, sqlx::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let r = sqlx::query(
            "INSERT OR IGNORE INTO characters (id, account_id, name, class, gender, level, zone, x, y)
             VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?)",
        )
        .bind(&id)
        .bind(account_id)
        .bind(name)
        .bind(class_to_str(class))
        .bind(gender)
        .bind(zone)
        .bind(spawn.0)
        .bind(spawn.1)
        .execute(&self.pool)
        .await?;
        Ok((r.rows_affected() > 0).then_some(id))
    }

    pub async fn characters_of(
        &self,
        account_id: &str,
    ) -> Result<Vec<CharacterSummary>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, name, class, gender, level FROM characters WHERE account_id = ?",
        )
        .bind(account_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| CharacterSummary {
                id: r.get("id"),
                name: r.get("name"),
                class: class_from_str(&r.get::<String, _>("class")),
                gender: r.get("gender"),
                level: r.get::<i64, _>("level") as u32,
            })
            .collect())
    }

    /// 取账号名下指定角色（防越权选角）
    pub async fn character(
        &self,
        account_id: &str,
        character_id: &str,
    ) -> Result<Option<CharacterRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, name, class, gender, level, exp, zone, x, y, inventory, equipment, quests FROM characters WHERE id = ? AND account_id = ?",
        )
        .bind(character_id)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| CharacterRow {
            id: r.get("id"),
            name: r.get("name"),
            class: class_from_str(&r.get::<String, _>("class")),
            gender: r.get("gender"),
            level: r.get::<i64, _>("level") as u32,
            exp: r.get::<i64, _>("exp") as u64,
            zone: r.get("zone"),
            inventory: serde_json::from_str(&r.get::<String, _>("inventory")).unwrap_or_default(),
            equipment: serde_json::from_str(&r.get::<String, _>("equipment")).unwrap_or_default(),
            quests: serde_json::from_str(&r.get::<String, _>("quests")).unwrap_or_default(),
            x: r.get("x"),
            y: r.get("y"),
        }))
    }

    pub async fn save_position(
        &self,
        character_id: &str,
        zone: &str,
        x: f64,
        y: f64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE characters SET zone = ?, x = ?, y = ? WHERE id = ?")
            .bind(zone)
            .bind(x)
            .bind(y)
            .bind(character_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn save_items(
        &self,
        character_id: &str,
        inventory: &[protocol::ItemInfo],
        equipment: &std::collections::HashMap<String, protocol::ItemInfo>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE characters SET inventory = ?, equipment = ? WHERE id = ?")
            .bind(serde_json::to_string(inventory).unwrap_or_else(|_| "[]".into()))
            .bind(serde_json::to_string(equipment).unwrap_or_else(|_| "{}".into()))
            .bind(character_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn save_quests(
        &self,
        character_id: &str,
        quests: &std::collections::HashMap<String, crate::game::QuestProgress>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE characters SET quests = ? WHERE id = ?")
            .bind(serde_json::to_string(quests).unwrap_or_else(|_| "{}".into()))
            .bind(character_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn save_progress(
        &self,
        character_id: &str,
        level: u32,
        exp: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE characters SET level = ?, exp = ? WHERE id = ?")
            .bind(level as i64)
            .bind(exp as i64)
            .bind(character_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_login_roundtrip() {
        let db = Db::open(":memory:").await.unwrap();
        let id = db.register("alice", "s3cret").await.unwrap().unwrap();
        // 重名拒绝
        assert!(db.register("alice", "other").await.unwrap().is_none());
        // 口令校验
        assert_eq!(db.login("alice", "s3cret").await.unwrap(), Some(id.clone()));
        assert_eq!(db.login("alice", "wrong").await.unwrap(), None);
        assert_eq!(db.login("nobody", "x").await.unwrap(), None);
        // 库里没有明文
        let row = sqlx::query("SELECT password_hash FROM accounts WHERE username='alice'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let hash: String = row.get(0);
        assert!(hash.starts_with("$2") && !hash.contains("s3cret"));
    }

    #[tokio::test]
    async fn character_lifecycle() {
        let db = Db::open(":memory:").await.unwrap();
        let acc = db.register("bob", "pw").await.unwrap().unwrap();
        let cid = db
            .create_character(
                &acc,
                "侠客",
                CharacterClass::Warrior,
                "male",
                "0.map",
                (330.5, 150.5),
            )
            .await
            .unwrap()
            .unwrap();
        // 重名拒绝
        assert!(db
            .create_character(
                &acc,
                "侠客",
                CharacterClass::Mage,
                "female",
                "0.map",
                (0.0, 0.0)
            )
            .await
            .unwrap()
            .is_none());
        let list = db.characters_of(&acc).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "侠客");
        // 越权取角失败
        assert!(db.character("someone-else", &cid).await.unwrap().is_none());
        let c = db.character(&acc, &cid).await.unwrap().unwrap();
        assert_eq!((c.x, c.y), (330.5, 150.5));
        db.save_position(&cid, "2.map", 331.0, 151.0).await.unwrap();
        let c = db.character(&acc, &cid).await.unwrap().unwrap();
        assert_eq!(c.zone, "2.map");
        let c = db.character(&acc, &cid).await.unwrap().unwrap();
        assert_eq!((c.x, c.y), (331.0, 151.0));
    }
}
