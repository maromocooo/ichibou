//! Looking inside backend nodes: the OpenAPI document of an API, a request
//! runner for trying it while it is being built, and the table structure of
//! a database.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use aperture_core::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::engine::Engine;

fn api_source(engine: &Engine, node: &str) -> Result<ApiSource> {
    engine
        .project
        .load()?
        .targets
        .apis
        .into_iter()
        .find(|a| a.node == node)
        .ok_or_else(|| anyhow!("no OpenAPI source is registered for `{node}` in targets.yaml"))
}

/// Resolves a path from targets.yaml and refuses anything that ends up
/// outside the project (absolute paths, `..`, symlinks pointing away).
fn inside_project(engine: &Engine, relative: &str) -> Result<std::path::PathBuf> {
    let root = engine.project.root.canonicalize()?;
    let full = root.join(relative).canonicalize()?;
    if !full.starts_with(&root) {
        bail!("`{relative}` is outside the project");
    }
    Ok(full)
}

fn parse_spec(text: &str) -> Result<Value> {
    // JSON is valid YAML, so one parser covers both.
    serde_yaml::from_str(text).context("the OpenAPI document is not valid JSON or YAML")
}

/// Loads the OpenAPI document for an API node.
/// targets.yaml sits in the workspace, so an agent can write URLs into it.
/// Nothing in it is fetched, connected to or run until the user has approved
/// that exact version of the file.
fn require_approval(engine: &Engine) -> Result<()> {
    if !engine.targets_approved() {
        bail!("approve targets.yaml first: its URLs and commands have changed since you last reviewed them");
    }
    Ok(())
}

pub async fn openapi(engine: &Engine, node: &str) -> Result<Value> {
    let source = api_source(engine, node)?;
    require_approval(engine)?;
    let spec = if let Some(file) = &source.openapi_file {
        let path = inside_project(engine, file)
            .with_context(|| format!("{file} does not exist yet"))?;
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("{file} does not exist yet"))?;
        parse_spec(&text)?
    } else if let Some(url) = &source.openapi_url {
        let text = reqwest::Client::new()
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .with_context(|| format!("cannot reach {url}; is the API running?"))?
            .error_for_status()?
            .text()
            .await?;
        parse_spec(&text)?
    } else if let Some(command) = &source.openapi_command {
        let out = tokio::time::timeout(
            Duration::from_secs(60),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(&engine.project.root)
                .output(),
        )
        .await
        .context("the OpenAPI command timed out")??;
        if !out.status.success() {
            bail!(
                "`{command}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        parse_spec(&String::from_utf8_lossy(&out.stdout))?
    } else {
        bail!("`{node}` has no openapi_file, openapi_url or openapi_command");
    };
    Ok(json!({ "spec": spec, "base_url": source.base_url }))
}

#[derive(Debug, Deserialize)]
pub struct TryRequest {
    pub method: String,
    /// Path and query, starting with `/`.
    pub path: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Sends a request to the API's registered development URL and returns the
/// response. Only that URL can be reached, so this is not an open proxy.
pub async fn try_request(engine: &Engine, node: &str, request: TryRequest) -> Result<Value> {
    let source = api_source(engine, node)?;
    require_approval(engine)?;
    let base = source
        .base_url
        .context("set `base_url` for this API in targets.yaml to send requests")?;
    if !request.path.starts_with('/') || request.path.starts_with("//") {
        bail!("the path must start with a single `/`");
    }
    let method: reqwest::Method = request
        .method
        .to_uppercase()
        .parse()
        .context("unknown HTTP method")?;
    let mut builder = reqwest::Client::new()
        .request(method, format!("{}{}", base.trim_end_matches('/'), request.path))
        .timeout(Duration::from_secs(30));
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    if let Some(body) = request.body {
        builder = builder.body(body);
    }
    let started = std::time::Instant::now();
    let response = builder
        .send()
        .await
        .with_context(|| format!("no answer from {base}; is the API running?"))?;
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let bytes = response.bytes().await?;
    let truncated = bytes.len() > 512 * 1024;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(512 * 1024)]).into_owned();
    Ok(json!({
        "status": status,
        "headers": headers,
        "body": body,
        "truncated": truncated,
        "duration_ms": started.elapsed().as_millis() as u64,
    }))
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ForeignKey {
    pub column: String,
    pub references_table: String,
    pub references_column: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Column>,
    pub foreign_keys: Vec<ForeignKey>,
}

/// Reads the structure of the database behind a node, as it exists now.
pub async fn database(engine: &Engine, node: &str) -> Result<Value> {
    let source = engine
        .project
        .load()?
        .targets
        .databases
        .into_iter()
        .find(|d| d.node == node)
        .ok_or_else(|| anyhow!("no database is registered for `{node}` in targets.yaml"))?;
    require_approval(engine)?;
    let tables = match source.kind {
        DatabaseKind::Sqlite => {
            let relative = source
                .url
                .trim_start_matches("sqlite://")
                .trim_start_matches("sqlite:");
            let path = inside_project(engine, relative)
                .with_context(|| format!("{relative} does not exist yet"))?;
            tokio::task::spawn_blocking(move || sqlite_tables(&path)).await??
        }
        DatabaseKind::Postgres => postgres_tables(&source.url).await?,
    };
    Ok(json!({ "kind": source.kind, "tables": tables }))
}

pub fn sqlite_tables(path: &std::path::Path) -> Result<Vec<Table>> {
    if !path.exists() {
        bail!("{} does not exist yet", path.display());
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut tables = Vec::new();
    for name in names {
        let quoted = name.replace('"', "\"\"");
        let columns = conn
            .prepare(&format!("PRAGMA table_info(\"{quoted}\")"))?
            .query_map([], |r| {
                let primary_key = r.get::<_, i64>(5)? > 0;
                Ok(Column {
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    // SQLite reports primary keys as nullable unless NOT NULL
                    // is spelled out; in practice they never hold NULL.
                    nullable: r.get::<_, i64>(3)? == 0 && !primary_key,
                    primary_key,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let foreign_keys = conn
            .prepare(&format!("PRAGMA foreign_key_list(\"{quoted}\")"))?
            .query_map([], |r| {
                Ok(ForeignKey {
                    references_table: r.get(2)?,
                    column: r.get(3)?,
                    references_column: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        tables.push(Table {
            name,
            columns,
            foreign_keys,
        });
    }
    Ok(tables)
}

async fn postgres_tables(url: &str) -> Result<Vec<Table>> {
    let (client, connection) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_postgres::connect(url, tokio_postgres::NoTls),
    )
    .await
    .context("the database did not answer")?
    .context("cannot connect to the database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let columns = client
        .query(
            "SELECT c.table_name, c.column_name, c.data_type, c.is_nullable = 'YES',
                    EXISTS (
                        SELECT 1 FROM information_schema.table_constraints tc
                        JOIN information_schema.key_column_usage k
                          ON k.constraint_name = tc.constraint_name AND k.table_schema = tc.table_schema
                        WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = c.table_schema
                          AND tc.table_name = c.table_name AND k.column_name = c.column_name)
             FROM information_schema.columns c
             JOIN information_schema.tables t
               ON t.table_schema = c.table_schema AND t.table_name = c.table_name
             WHERE c.table_schema = current_schema() AND t.table_type = 'BASE TABLE'
             ORDER BY c.table_name, c.ordinal_position",
            &[],
        )
        .await?;
    let keys = client
        .query(
            "SELECT k.table_name, k.column_name, u.table_name, u.column_name
             FROM information_schema.table_constraints tc
             JOIN information_schema.key_column_usage k
               ON k.constraint_name = tc.constraint_name AND k.table_schema = tc.table_schema
             JOIN information_schema.constraint_column_usage u
               ON u.constraint_name = tc.constraint_name AND u.table_schema = tc.table_schema
             WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = current_schema()",
            &[],
        )
        .await?;

    let mut tables: Vec<Table> = Vec::new();
    for row in columns {
        let table: String = row.get(0);
        if tables.last().map(|t| &t.name) != Some(&table) {
            tables.push(Table {
                name: table,
                columns: vec![],
                foreign_keys: vec![],
            });
        }
        tables.last_mut().unwrap().columns.push(Column {
            name: row.get(1),
            kind: row.get(2),
            nullable: row.get(3),
            primary_key: row.get(4),
        });
    }
    for row in keys {
        let table: String = row.get(0);
        if let Some(found) = tables.iter_mut().find(|t| t.name == table) {
            found.foreign_keys.push(ForeignKey {
                column: row.get(1),
                references_table: row.get(2),
                references_column: row.get(3),
            });
        }
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_structure_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL);
             CREATE TABLE posts (id INTEGER PRIMARY KEY, author_id INTEGER REFERENCES users(id), body TEXT);",
        )
        .unwrap();
        drop(conn);
        let tables = sqlite_tables(&path).unwrap();
        assert_eq!(tables.len(), 2);
        let posts = &tables[0];
        assert_eq!(posts.name, "posts");
        assert_eq!(
            posts.foreign_keys,
            vec![ForeignKey {
                column: "author_id".into(),
                references_table: "users".into(),
                references_column: "id".into()
            }]
        );
        let email = &tables[1].columns[1];
        assert!(!email.nullable && !email.primary_key);
        let id = &tables[1].columns[0];
        assert!(id.primary_key && !id.nullable);
        assert!(tables[0].columns[2].nullable);
        assert!(sqlite_tables(&dir.path().join("missing.db")).is_err());
    }

    #[test]
    fn spec_accepts_json_and_yaml() {
        assert_eq!(parse_spec(r#"{"openapi":"3.1.0"}"#).unwrap()["openapi"], "3.1.0");
        assert_eq!(parse_spec("openapi: 3.1.0\npaths: {}\n").unwrap()["openapi"], "3.1.0");
    }
}
