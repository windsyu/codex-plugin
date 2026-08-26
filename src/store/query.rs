use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;

pub(super) fn query_json_connection(
    connection: &Connection,
    sql: &str,
    parameters: &[&dyn rusqlite::ToSql],
    mapper: fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
) -> Result<Vec<Value>> {
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map(parameters, mapper)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
