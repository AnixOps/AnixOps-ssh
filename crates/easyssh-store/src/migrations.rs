//! Numbered migrations recorded in `schema_migrations`.

use crate::error::{Result, StoreError};
use crate::meta::fmt_ts;
use chrono::Utc;
use rusqlite::Connection;

struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "host_shell_integration",
        sql: include_str!("../migrations/0002_host_shell_integration.sql"),
    },
    Migration {
        version: 3,
        name: "host_secrets",
        sql: include_str!("../migrations/0003_host_secrets.sql"),
    },
    Migration {
        version: 4,
        name: "sync",
        sql: include_str!("../migrations/0004_sync.sql"),
    },
    Migration {
        version: 5,
        name: "host_engine",
        sql: include_str!("../migrations/0005_host_engine.sql"),
    },
];

/// The newest schema version this build can apply.
pub fn latest_version() -> i64 {
    MIGRATIONS.last().map_or(0, |migration| migration.version)
}

fn has_table(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

pub(crate) fn current_version(conn: &Connection) -> Result<i64> {
    if !has_table(conn)? {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

/// Fails if the database was written by a newer build.
pub(crate) fn refuse_newer(conn: &Connection) -> Result<()> {
    let found = current_version(conn)?;
    if found > latest_version() {
        return Err(StoreError::DatabaseTooNew {
            found,
            supported: latest_version(),
        });
    }
    Ok(())
}

/// Applies every migration newer than the database. Running it again does
/// nothing. Each migration and its bookkeeping row commit together.
pub(crate) fn apply(conn: &Connection) -> Result<()> {
    for migration in MIGRATIONS {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            // Re-check under the write lock in case another process won.
            refuse_newer(conn)?;
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    applied_at TEXT NOT NULL
                )",
            )?;
            if current_version(conn)? >= migration.version {
                return Ok(());
            }
            conn.execute_batch(migration.sql)?;
            conn.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![migration.version, migration.name, fmt_ts(Utc::now())],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
    Ok(())
}
