//! `{app_data_dir}/susurro.db`: open + schema migrations by `PRAGMA user_version`.
//! Each module writes its own queries.

use rusqlite::Connection;
use std::path::Path;

/// Index i holds the step from version i to i+1. Append only; never edit a shipped step.
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE history(id INTEGER PRIMARY KEY, created_at INTEGER NOT NULL, app_bundle TEXT, app_name TEXT,
  raw TEXT NOT NULL, final_text TEXT NOT NULL, provider TEXT NOT NULL, cleanup TEXT,
  duration_ms INTEGER NOT NULL, words INTEGER NOT NULL, latency_ms INTEGER NOT NULL,
  status TEXT NOT NULL, audio TEXT, error TEXT);
CREATE INDEX history_created ON history(created_at);
CREATE TABLE dict(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, text TEXT NOT NULL, norm TEXT NOT NULL UNIQUE,
  aliases TEXT NOT NULL DEFAULT '[]', expansion TEXT, exact_only INTEGER NOT NULL DEFAULT 0,
  starred INTEGER NOT NULL DEFAULT 0, uses INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);
"#];

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&conn)?;
    Ok(conn)
}

/// In-memory DB with the full schema, for tests in any module.
pub fn open_memory() -> anyhow::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v as usize) {
        conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {}; COMMIT;", i + 1))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_db_gets_schema_and_migrate_is_idempotent() {
        let c = open_memory().unwrap();
        let v: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
        migrate(&c).unwrap();
        c.execute(
            "INSERT INTO history(created_at,raw,final_text,provider,duration_ms,words,latency_ms,status) VALUES(1,'a','A','whisper_local',100,1,5,'pasted')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO dict(kind,text,norm,created_at) VALUES('word','Contaflow','contaflow',1)", []).unwrap();
        assert!(c.execute("INSERT INTO dict(kind,text,norm,created_at) VALUES('word','ContaFlow','contaflow',2)", []).is_err());
    }

    #[test]
    fn file_db_reopens() {
        let p = std::env::temp_dir().join(format!("susurro-test-db-{}/susurro.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        drop(open(&p).unwrap());
        let c = open(&p).unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM history", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
