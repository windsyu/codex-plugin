use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn secret() -> String {
    let mut b = [0; 32];
    rand::rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}
pub fn digest(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}
pub struct Store(pub Mutex<Connection>, AtomicI64);
#[derive(Clone)]
pub struct Session {
    pub id: Uuid,
    pub account: Uuid,
    pub gateway: Option<Uuid>,
}
impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        secure_files(dir)?;
        // Resolve OS aliases such as macOS /var only after rejecting a linked configured root.
        let dir = std::fs::canonicalize(dir)?;
        let c = Connection::open_with_flags(
            dir.join("relay.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            matches!(version, 0 | 1),
            "unsupported future relay database schema"
        );
        c.busy_timeout(std::time::Duration::from_secs(2))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY, username TEXT UNIQUE NOT NULL, password TEXT, disabled INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), kind TEXT NOT NULL, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS gateways(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), name TEXT NOT NULL, credential TEXT UNIQUE NOT NULL);
        CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, hash TEXT UNIQUE NOT NULL, account TEXT NOT NULL REFERENCES accounts(id), gateway TEXT REFERENCES gateways(id) ON DELETE CASCADE, parent TEXT REFERENCES sessions(id) ON DELETE CASCADE, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS open_codes(hash TEXT PRIMARY KEY, parent TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, gateway TEXT NOT NULL REFERENCES gateways(id) ON DELETE CASCADE, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, event TEXT NOT NULL, account TEXT, gateway TEXT);
        CREATE INDEX IF NOT EXISTS sessions_expires ON sessions(expires);
        CREATE INDEX IF NOT EXISTS sessions_parent ON sessions(parent);
        CREATE INDEX IF NOT EXISTS tokens_expires ON tokens(expires);
        CREATE INDEX IF NOT EXISTS open_codes_expires ON open_codes(expires);
        CREATE INDEX IF NOT EXISTS open_codes_parent ON open_codes(parent);
        PRAGMA user_version=1; COMMIT;")?;
        cleanup_expired_batch(&c, now())?;
        Ok(Self(Mutex::new(c), AtomicI64::new(now() + 60)))
    }
    /// Opportunistic bounded maintenance, shared by all control connections.
    /// A contended account store is skipped instead of delaying a forwarding task.
    pub(crate) fn cleanup_expired_if_due(&self) -> rusqlite::Result<()> {
        let time = now();
        let due = self.1.load(Ordering::Relaxed);
        if time < due
            || self
                .1
                .compare_exchange(due, time + 60, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return Ok(());
        }
        let Ok(c) = self.0.try_lock() else {
            return Ok(());
        };
        cleanup_expired_batch(&c, time)?;
        Ok(())
    }
    pub fn issue(&self, username: &str, reset: bool) -> Result<String> {
        anyhow::ensure!(
            !username.is_empty()
                && username.len() <= 64
                && username
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-.".contains(&c)),
            "invalid username"
        );
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let account = if reset {
            tx.query_row(
                "SELECT id FROM accounts WHERE username=?1 AND disabled=0",
                [username],
                |r| r.get::<_, String>(0),
            )?
        } else {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO accounts(id,username) VALUES(?1,?2)",
                params![id, username],
            )?;
            id
        };
        tx.execute("DELETE FROM tokens WHERE account=?1", [&account])?;
        let token = secret();
        tx.execute(
            "INSERT INTO tokens VALUES(?1,?2,?3,?4)",
            params![
                digest(&token),
                account,
                if reset { "reset" } else { "invite" },
                now() + if reset { 900 } else { 86400 }
            ],
        )?;
        audit(
            &tx,
            if reset {
                "reset_issued"
            } else {
                "invite_issued"
            },
            Some(&account),
            None,
        )?;
        tx.commit()?;
        Ok(token)
    }
    pub fn disable(&self, username: &str) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction()?;
        let account: Option<String> = tx
            .query_row(
                "SELECT id FROM accounts WHERE username=?1",
                [username],
                |r| r.get(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE accounts SET disabled=1 WHERE username=?1",
            [username],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE account IN (SELECT id FROM accounts WHERE username=?1)",
            [username],
        )?;
        tx.execute(
            "DELETE FROM tokens WHERE account IN (SELECT id FROM accounts WHERE username=?1)",
            [username],
        )?;
        if let Some(account) = account {
            audit(&tx, "disabled", Some(&account), None)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn session(&self, token: &str, gateway: Option<Uuid>) -> Option<Session> {
        let c = self.0.lock().ok()?;
        let (id,account,g) = c.query_row("SELECT s.id,s.account,s.gateway FROM sessions s JOIN accounts a ON a.id=s.account WHERE s.hash=?1 AND s.expires>?2 AND a.disabled=0 AND (s.parent IS NULL OR EXISTS(SELECT 1 FROM sessions p WHERE p.id=s.parent AND p.expires>?2))", params![digest(token),now()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).optional().ok()??;
        Some(Session {
            id: Uuid::parse_str(&id).ok()?,
            account: Uuid::parse_str(&account).ok()?,
            gateway: g.map(|s| Uuid::parse_str(&s)).transpose().ok()?,
        })
        .filter(|s| s.gateway == gateway)
    }
    pub fn active(&self, id: Uuid) -> bool {
        self.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sessions s JOIN accounts a ON a.id=s.account WHERE s.id=?1 AND s.expires>?2 AND a.disabled=0 AND (s.parent IS NULL OR EXISTS(SELECT 1 FROM sessions p WHERE p.id=s.parent AND p.expires>?2)))", params![id.to_string(),now()], |r|r.get(0)).unwrap_or(false)
    }
    pub fn gateway_auth(&self, token: &str) -> Option<(Uuid, Uuid)> {
        let c = self.0.lock().ok()?;
        let (g,a) = c.query_row("SELECT g.id,g.account FROM gateways g JOIN accounts a ON a.id=g.account WHERE g.credential=?1 AND a.disabled=0",[digest(token)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().ok()??;
        Some((Uuid::parse_str(&g).ok()?, Uuid::parse_str(&a).ok()?))
    }
}

/// Fixed event names and UUID identifiers only. No request, identity text or credential payloads.
pub(crate) fn audit(
    c: &Connection,
    event: &str,
    account: Option<&str>,
    gateway: Option<&str>,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO audit(at,event,account,gateway) VALUES(?1,?2,?3,?4)",
        params![now(), event, account, gateway],
    )?;
    c.execute("DELETE FROM audit WHERE at<?1 OR id NOT IN (SELECT id FROM audit ORDER BY id DESC LIMIT 10000)",[now()-90*86400])?;
    Ok(())
}
#[cfg(unix)]
fn secure_files(dir: &Path) -> Result<()> {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    // The configured root itself may not redirect to another directory.
    match fs::symlink_metadata(dir) {
        Ok(m) => anyhow::ensure!(
            m.is_dir() && !m.file_type().is_symlink(),
            "relay data root is not a regular directory"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Err(e) => return Err(e.into()),
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)?;
    // SAFETY: geteuid has no pointer arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        directory.metadata()?.uid() == uid,
        "relay data root has a different owner"
    );
    directory.set_permissions(fs::Permissions::from_mode(0o700))?;
    for name in [
        "relay.sqlite3",
        "relay.sqlite3-wal",
        "relay.sqlite3-shm",
        "relay.sqlite3-journal",
    ] {
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(m) => anyhow::ensure!(
                m.is_file() && !m.file_type().is_symlink() && m.nlink() == 1 && m.uid() == uid,
                "unsafe relay database file"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if name != "relay.sqlite3" {
                    continue;
                }
            }
            Err(e) => return Err(e.into()),
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(name == "relay.sqlite3")
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let m = file.metadata()?;
        anyhow::ensure!(
            m.is_file() && m.nlink() == 1 && m.uid() == uid,
            "unsafe relay database file"
        );
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn secure_files(_dir: &Path) -> Result<()> {
    anyhow::bail!("relay requires a Unix service host")
}

/// Every statement removes at most 128 rows. Clear dependent rows first and only
/// delete leaf sessions, so a highly populated expired parent cannot turn the
/// bounded batch into an unbounded foreign-key cascade. The next sweep resumes
/// any remaining dependents. All lookups use expiry/parent indexes.
pub(crate) fn cleanup_expired_batch(c: &Connection, time: i64) -> rusqlite::Result<usize> {
    let mut deleted=c.execute("DELETE FROM open_codes WHERE hash IN (
        SELECT hash FROM open_codes WHERE expires<=?1
        OR parent IN (SELECT id FROM sessions WHERE expires<=?1 LIMIT 128)
        OR parent IN (SELECT id FROM sessions WHERE parent IN (SELECT id FROM sessions WHERE expires<=?1 LIMIT 128) LIMIT 128)
        LIMIT 128)",[time])?;
    deleted+=c.execute("DELETE FROM sessions WHERE id IN (
        SELECT s.id FROM sessions s WHERE s.id IN (SELECT id FROM sessions WHERE parent IN (SELECT id FROM sessions WHERE expires<=?1 LIMIT 128) LIMIT 128)
        AND NOT EXISTS(SELECT 1 FROM sessions child WHERE child.parent=s.id)
        AND NOT EXISTS(SELECT 1 FROM open_codes o WHERE o.parent=s.id)
        LIMIT 128)",[time])?;
    deleted += c.execute(
        "DELETE FROM sessions WHERE id IN (
        SELECT s.id FROM sessions s WHERE s.id IN (SELECT id FROM sessions WHERE expires<=?1 LIMIT 128)
        AND NOT EXISTS(SELECT 1 FROM sessions child WHERE child.parent=s.id)
        AND NOT EXISTS(SELECT 1 FROM open_codes o WHERE o.parent=s.id)
        LIMIT 128)",
        [time],
    )?;
    deleted += c.execute(
        "DELETE FROM tokens WHERE hash IN (SELECT hash FROM tokens WHERE expires<=?1 LIMIT 128)",
        [time],
    )?;
    Ok(deleted)
}
