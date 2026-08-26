use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};

pub(super) struct ReadPool {
    path: PathBuf,
    max: usize,
    state: Mutex<ReadPoolState>,
    available: Condvar,
}

struct ReadPoolState {
    idle: Vec<Connection>,
    total: usize,
}

pub(super) struct ReadConnection<'a> {
    pool: &'a ReadPool,
    connection: Option<Connection>,
}

impl ReadPool {
    pub(super) fn new(path: PathBuf, max: usize) -> Self {
        Self {
            path,
            max,
            state: Mutex::new(ReadPoolState {
                idle: Vec::new(),
                total: 0,
            }),
            available: Condvar::new(),
        }
    }

    pub(super) fn get(&self) -> Result<ReadConnection<'_>> {
        loop {
            let mut state = self.state.lock().expect("read pool poisoned");
            if let Some(connection) = state.idle.pop() {
                return Ok(ReadConnection {
                    pool: self,
                    connection: Some(connection),
                });
            }
            if state.total < self.max {
                state.total += 1;
                drop(state);
                match Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                    Ok(connection) => {
                        connection.busy_timeout(std::time::Duration::from_secs(5))?;
                        connection.pragma_update(None, "query_only", true)?;
                        return Ok(ReadConnection {
                            pool: self,
                            connection: Some(connection),
                        });
                    }
                    Err(error) => {
                        let mut state = self.state.lock().expect("read pool poisoned");
                        state.total = state.total.saturating_sub(1);
                        self.available.notify_one();
                        return Err(error.into());
                    }
                }
            }
            drop(self.available.wait(state).expect("read pool poisoned"));
        }
    }
}

impl Deref for ReadConnection<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("read connection returned once")
    }
}

impl Drop for ReadConnection<'_> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let mut state = self.pool.state.lock().expect("read pool poisoned");
            state.idle.push(connection);
            self.pool.available.notify_one();
        }
    }
}
