use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

/// ذخیره‌ی کاربران در SQLite.
/// `Connection` مقدار `Sync` نیست؛ با Mutex می‌شه بین تسک‌ها به اشتراک گذاشت.
/// متدها هم‌زمان (sync) و کوتاه‌اند و guard هیچ‌وقت across `.await` نگه داشته نمی‌شود.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn new(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS users (
                chat_id    INTEGER PRIMARY KEY,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
            [],
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // اگه thread دیگه‌ای وسط کار panic کرده باشه، دیتابیس همچنان قابل استفاده‌ست
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// true اگر کاربر جدید بود
    pub fn add_user(&self, chat_id: i64) -> anyhow::Result<bool> {
        let n = self
            .conn()
            .execute("INSERT OR IGNORE INTO users (chat_id) VALUES (?1)", [chat_id])?;
        Ok(n > 0)
    }

    pub fn remove_user(&self, chat_id: i64) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM users WHERE chat_id = ?1", [chat_id])?;
        Ok(())
    }

    pub fn all_users(&self) -> anyhow::Result<Vec<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT chat_id FROM users")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_list() {
        let db = Db::new(":memory:").unwrap();
        assert!(db.add_user(1).unwrap());
        assert!(!db.add_user(1).unwrap()); // تکراری
        assert!(db.add_user(2).unwrap());
        let mut all = db.all_users().unwrap();
        all.sort();
        assert_eq!(all, vec![1, 2]);
        db.remove_user(1).unwrap();
        assert_eq!(db.all_users().unwrap(), vec![2]);
    }
}
