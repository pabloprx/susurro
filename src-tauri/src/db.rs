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
"#,
// v2: dict.created_at in ms like every other timestamp (v1 stored seconds).
"UPDATE dict SET created_at = created_at * 1000 WHERE created_at < 100000000000;",
// v3: voice command rows next to dictations.
"ALTER TABLE history ADD COLUMN kind TEXT NOT NULL DEFAULT 'dictation';",
// v4: default dictionary words; `dictionary::seed` runs right after this step.
"SELECT 1;",
// v5: "PR" and "PRs"; `dictionary::seed_v5` runs right after this step.
"SELECT 1;"];
const SEED_STEP: usize = 3;
const SEED_STEP_V5: usize = 4;

pub fn open(path: &Path) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&conn)?;
    Ok(conn)
}

/// In-memory DB with the full schema and an empty dictionary (no seeds), for tests in any module.
pub fn open_memory() -> anyhow::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    conn.execute("DELETE FROM dict", [])?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v as usize) {
        conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {}; COMMIT;", i + 1))?;
        if i == SEED_STEP {
            crate::dictionary::seed(conn);
        }
        if i == SEED_STEP_V5 {
            crate::dictionary::seed_v5(conn);
        }
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
    fn v2_turns_dict_seconds_into_ms() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("{} PRAGMA user_version = 1;", MIGRATIONS[0])).unwrap();
        c.execute("INSERT INTO dict(kind,text,norm,created_at) VALUES('word','A','a',1760000000),('word','B','b',1760000000123)", []).unwrap();
        migrate(&c).unwrap();
        let v: Vec<i64> = c.prepare("SELECT created_at FROM dict WHERE text IN ('A', 'B') ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
        assert_eq!(v, [1760000000000, 1760000000123]);
    }

    #[test]
    fn v4_seeds_only_absent_words_once() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("{} {} {} PRAGMA user_version = 3;", MIGRATIONS[0], MIGRATIONS[1], MIGRATIONS[2])).unwrap();
        // Pablo's own Contaflow entry (other casing, a correction) and "x d" as a correction of something else.
        c.execute("INSERT INTO dict(kind,text,norm,aliases,created_at) VALUES('word','ContaFlow','contaflow','[\"conta flow\"]',1),('word','XD!','xd','[]',2)", []).unwrap();
        migrate(&c).unwrap();
        let es = crate::dictionary::load(&c).unwrap();
        let texts: Vec<&str> = es.iter().map(|e| e.text.as_str()).collect();
        assert!(texts.contains(&"ContaFlow") && !texts.contains(&"Contaflow"), "his entry stays: {texts:?}");
        assert!(texts.contains(&"XD!") && !texts.contains(&"xd"), "{texts:?}");
        for t in ["Jev", "TypeSafe", "ElevenLabs", "67", "cachai"] {
            assert!(texts.contains(&t), "{t} seeded: {texts:?}");
        }
        assert_eq!(es.iter().find(|e| e.text == "67").unwrap().aliases, ["six seven", "seis siete"]);
        // Deleted later = not seeded again.
        c.execute("DELETE FROM dict WHERE text = 'Jev'", []).unwrap();
        migrate(&c).unwrap();
        assert!(!crate::dictionary::load(&c).unwrap().iter().any(|e| e.text == "Jev"));
    }

    #[test]
    fn v5_seeds_pr_starred_on_a_v4_db() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("{} {} {} {} PRAGMA user_version = 4;", MIGRATIONS[0], MIGRATIONS[1], MIGRATIONS[2], MIGRATIONS[3])).unwrap();
        migrate(&c).unwrap();
        let es = crate::dictionary::load(&c).unwrap();
        let pr = es.iter().find(|e| e.text == "PR").expect("PR seeded");
        assert!(pr.starred && pr.aliases.contains(&"pe erre".to_string()), "{pr:?}");
        assert!(es.iter().any(|e| e.text == "PRs"));
        assert!(!es.iter().any(|e| e.text == "Jev"), "v4 seeds do not run again on a v4 db");
        let d = crate::dictionary::Dictionary::new(es, Default::default());
        assert_eq!(d.hints().terms.first().map(String::as_str), Some("PR"), "starred: first in the Whisper prompt");
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
