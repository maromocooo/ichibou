//! Local state in SQLite (`.ichibou/local/state.db`).
//!
//! Records are stored as JSON documents keyed by id; the columns next to them
//! exist only for lookup and ordering. Everything survives a daemon restart.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::model::*;

/// Bump when adding a migration.
const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE events (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        at TEXT NOT NULL,
        run TEXT,
        kind TEXT NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE INDEX events_run ON events(run, seq);
    CREATE TABLE runs (id TEXT PRIMARY KEY, doc TEXT NOT NULL);
    CREATE TABLE claims (
        run TEXT NOT NULL,
        requirement TEXT NOT NULL,
        doc TEXT NOT NULL,
        PRIMARY KEY (run, requirement)
    );
    CREATE TABLE results (
        id TEXT PRIMARY KEY,
        scenario TEXT NOT NULL,
        at TEXT NOT NULL,
        doc TEXT NOT NULL
    );
    CREATE INDEX results_scenario ON results(scenario, at);
    CREATE TABLE approvals (
        scenario TEXT PRIMARY KEY,
        doc TEXT NOT NULL,
        frozen TEXT NOT NULL
    );
    CREATE TABLE confirmations (
        requirement TEXT NOT NULL,
        check_id TEXT NOT NULL,
        doc TEXT NOT NULL,
        PRIMARY KEY (requirement, check_id)
    );
    CREATE TABLE annotations (
        id TEXT PRIMARY KEY,
        created_at TEXT NOT NULL,
        doc TEXT NOT NULL
    );
    CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    ",
];

/// The approved scenario entry and the content of its test files, kept so
/// the runner can execute exactly what the user approved even after an agent
/// edits the working tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenScenario {
    pub scenario: Scenario,
    /// Relative path -> file content. `None` for a file that did not exist.
    pub files: HashMap<String, Option<Vec<u8>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredEvent {
    pub seq: i64,
    pub at: DateTime<Utc>,
    pub run: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn to_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn from_json<T: DeserializeOwned>(text: String) -> Result<T> {
    serde_json::from_str(&text).context("corrupt record in state database")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > MIGRATIONS.len() {
            bail!("state database was written by a newer version of Ichibou");
        }
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
            conn.execute_batch(&format!(
                "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
                index + 1
            ))?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn all<T: DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|row| from_json(row?)).collect()
    }

    // -- events ------------------------------------------------------------

    pub fn append_event(
        &self,
        run: Option<&str>,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO events (at, run, kind, payload) VALUES (?1, ?2, ?3, ?4)",
            params![Utc::now().to_rfc3339(), run, kind, payload.to_string()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn events_after(&self, seq: i64, limit: usize) -> Result<Vec<StoredEvent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, at, run, kind, payload FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![seq, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at, run, kind, payload) = row?;
            out.push(StoredEvent {
                seq,
                at: DateTime::parse_from_rfc3339(&at)?.with_timezone(&Utc),
                run,
                kind,
                payload: serde_json::from_str(&payload)?,
            });
        }
        Ok(out)
    }

    /// Most recent events, oldest first.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<StoredEvent>> {
        let max: i64 = self
            .conn()
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))?;
        self.events_after((max - limit as i64).max(0), limit)
    }

    // -- runs --------------------------------------------------------------

    pub fn put_run(&self, run: &Run) -> Result<()> {
        self.conn().execute(
            "INSERT INTO runs (id, doc) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET doc = excluded.doc",
            params![run.id, to_json(run)?],
        )?;
        Ok(())
    }

    pub fn run(&self, id: &str) -> Result<Option<Run>> {
        let doc: Option<String> = self
            .conn()
            .query_row("SELECT doc FROM runs WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        doc.map(from_json).transpose()
    }

    pub fn runs(&self) -> Result<Vec<Run>> {
        let mut runs: Vec<Run> = self.all("SELECT doc FROM runs")?;
        runs.sort_by_key(|r| r.started_at);
        Ok(runs)
    }

    // -- writer ownership --------------------------------------------------

    /// Current ownership generation of the workspace.
    pub fn generation(&self) -> Result<u64> {
        let value: Option<String> = self
            .conn()
            .query_row("SELECT value FROM kv WHERE key = 'generation'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    /// Hands the workspace to a new writer. Anything still arriving from a run
    /// with an older generation must be dropped by the caller.
    pub fn next_generation(&self) -> Result<u64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv (key, value) VALUES ('generation', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        )?;
        let value: String =
            conn.query_row("SELECT value FROM kv WHERE key = 'generation'", [], |r| {
                r.get(0)
            })?;
        Ok(value.parse()?)
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // -- claims ------------------------------------------------------------

    pub fn put_claim(&self, claim: &Claim) -> Result<()> {
        self.conn().execute(
            "INSERT INTO claims (run, requirement, doc) VALUES (?1, ?2, ?3)
             ON CONFLICT(run, requirement) DO UPDATE SET doc = excluded.doc",
            params![claim.run, claim.requirement, to_json(claim)?],
        )?;
        Ok(())
    }

    pub fn claims(&self) -> Result<Vec<Claim>> {
        self.all("SELECT doc FROM claims")
    }

    pub fn clear_claims(&self, run: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM claims WHERE run = ?1", [run])?;
        Ok(())
    }

    // -- verification ------------------------------------------------------

    pub fn add_result(&self, result: &VerificationResult) -> Result<()> {
        self.conn().execute(
            "INSERT INTO results (id, scenario, at, doc) VALUES (?1, ?2, ?3, ?4)",
            params![
                result.id,
                result.scenario,
                result.at.to_rfc3339(),
                to_json(result)?
            ],
        )?;
        Ok(())
    }

    pub fn results(&self) -> Result<Vec<VerificationResult>> {
        self.all("SELECT doc FROM results ORDER BY at")
    }

    /// Keeps the newest `keep` results per scenario and returns the artifact
    /// directories of the ones removed, for the caller to delete.
    pub fn prune_results(&self, keep: usize) -> Result<Vec<String>> {
        let all = self.results()?;
        let mut by_scenario: HashMap<&str, Vec<&VerificationResult>> = HashMap::new();
        for result in &all {
            by_scenario.entry(&result.scenario).or_default().push(result);
        }
        let mut removed = Vec::new();
        let conn = self.conn();
        for results in by_scenario.values() {
            let excess = results.len().saturating_sub(keep);
            for result in &results[..excess] {
                conn.execute("DELETE FROM results WHERE id = ?1", [&result.id])?;
                removed.extend(result.artifacts.clone());
            }
        }
        Ok(removed)
    }

    pub fn approve_scenario(&self, approval: &ScenarioApproval, frozen: &FrozenScenario) -> Result<()> {
        self.conn().execute(
            "INSERT INTO approvals (scenario, doc, frozen) VALUES (?1, ?2, ?3)
             ON CONFLICT(scenario) DO UPDATE SET doc = excluded.doc, frozen = excluded.frozen",
            params![approval.scenario, to_json(approval)?, to_json(frozen)?],
        )?;
        Ok(())
    }

    pub fn approvals(&self) -> Result<Vec<ScenarioApproval>> {
        self.all("SELECT doc FROM approvals")
    }

    pub fn frozen_scenario(&self, scenario: &str) -> Result<Option<FrozenScenario>> {
        let doc: Option<String> = self
            .conn()
            .query_row(
                "SELECT frozen FROM approvals WHERE scenario = ?1",
                [scenario],
                |r| r.get(0),
            )
            .optional()?;
        doc.map(from_json).transpose()
    }

    /// Every approved scenario entry with its frozen files.
    pub fn frozen_scenarios(&self) -> Result<Vec<FrozenScenario>> {
        self.all("SELECT frozen FROM approvals")
    }

    pub fn revoke_approval(&self, scenario: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM approvals WHERE scenario = ?1", [scenario])?;
        Ok(())
    }

    pub fn confirm_manual(&self, confirmation: &ManualConfirmation) -> Result<()> {
        self.conn().execute(
            "INSERT INTO confirmations (requirement, check_id, doc) VALUES (?1, ?2, ?3)
             ON CONFLICT(requirement, check_id) DO UPDATE SET doc = excluded.doc",
            params![
                confirmation.requirement,
                confirmation.check,
                to_json(confirmation)?
            ],
        )?;
        Ok(())
    }

    pub fn unconfirm_manual(&self, requirement: &str, check: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM confirmations WHERE requirement = ?1 AND check_id = ?2",
            params![requirement, check],
        )?;
        Ok(())
    }

    pub fn confirmations(&self) -> Result<Vec<ManualConfirmation>> {
        self.all("SELECT doc FROM confirmations")
    }

    // -- annotations -------------------------------------------------------

    pub fn put_annotation(&self, annotation: &Annotation) -> Result<()> {
        self.conn().execute(
            "INSERT INTO annotations (id, created_at, doc) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET doc = excluded.doc",
            params![
                annotation.id,
                annotation.created_at.to_rfc3339(),
                to_json(annotation)?
            ],
        )?;
        Ok(())
    }

    pub fn annotation(&self, id: &str) -> Result<Option<Annotation>> {
        let doc: Option<String> = self
            .conn()
            .query_row("SELECT doc FROM annotations WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        doc.map(from_json).transpose()
    }

    pub fn annotations(&self) -> Result<Vec<Annotation>> {
        self.all("SELECT doc FROM annotations ORDER BY created_at")
    }

    /// Applies `change` to one annotation inside a transaction-sized lock, so
    /// concurrent updates (an agent ack racing a user comment) do not lose
    /// each other's write. Returns the updated annotation.
    pub fn update_annotation(
        &self,
        id: &str,
        change: impl FnOnce(&mut Annotation),
    ) -> Result<Option<Annotation>> {
        let conn = self.conn();
        let doc: Option<String> = conn
            .query_row("SELECT doc FROM annotations WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(doc) = doc else { return Ok(None) };
        let mut annotation: Annotation = from_json(doc)?;
        change(&mut annotation);
        conn.execute(
            "UPDATE annotations SET doc = ?2 WHERE id = ?1",
            params![id, to_json(&annotation)?],
        )?;
        Ok(Some(annotation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotation(id: &str) -> Annotation {
        Annotation {
            id: id.into(),
            intent: AnnotationIntent::Fix,
            state: AnnotationState::Queued,
            anchor: Anchor::ArchNode { node: "api".into() },
            body: "rename this".into(),
            snapshot: SnapshotId("s1".into()),
            requirements: vec![],
            image: None,
            created_at: Utc::now(),
            comments: vec![],
        }
    }

    #[test]
    fn state_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local/state.db");
        {
            let store = Store::open(&path).unwrap();
            store.put_annotation(&annotation("a1")).unwrap();
            assert_eq!(store.next_generation().unwrap(), 1);
            assert_eq!(store.next_generation().unwrap(), 2);
            store
                .append_event(Some("r1"), "note", &serde_json::json!({"x": 1}))
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.annotations().unwrap().len(), 1);
        assert_eq!(store.generation().unwrap(), 2);
        assert_eq!(store.recent_events(10).unwrap()[0].kind, "note");
    }

    #[test]
    fn update_annotation_is_read_modify_write() {
        let store = Store::in_memory().unwrap();
        store.put_annotation(&annotation("a1")).unwrap();
        let updated = store
            .update_annotation("a1", |a| a.state = AnnotationState::Acknowledged)
            .unwrap()
            .unwrap();
        assert_eq!(updated.state, AnnotationState::Acknowledged);
        assert!(store.update_annotation("missing", |_| {}).unwrap().is_none());
    }

    #[test]
    fn prune_keeps_newest_per_scenario() {
        let store = Store::in_memory().unwrap();
        for i in 0..5 {
            store
                .add_result(&VerificationResult {
                    id: format!("r{i}"),
                    scenario: "s".into(),
                    outcome: Outcome::Passed,
                    snapshot: SnapshotId("x".into()),
                    definition: "d".into(),
                    at: Utc::now() + chrono::Duration::seconds(i),
                    detail: String::new(),
                    artifacts: Some(format!("art{i}")),
                })
                .unwrap();
        }
        let removed = store.prune_results(2).unwrap();
        assert_eq!(removed, vec!["art0", "art1", "art2"]);
        let left: Vec<_> = store.results().unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(left, vec!["r3", "r4"]);
    }
}
