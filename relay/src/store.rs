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

pub(crate) const ADMIN_USERNAME: &str = "windsyu";

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
    pub auth_generation: i64,
    pub must_change_password: bool,
    pub role: String,
}
impl Store {
    /// Checks the reserved bootstrap identity without changing it or reading any password file.
    pub fn has_admin(&self) -> Result<bool> {
        let c = self.0.lock().unwrap();
        let role: Option<String> = c
            .query_row(
                "SELECT role FROM accounts WHERE username=?1",
                [ADMIN_USERNAME],
                |r| r.get(0),
            )
            .optional()?;
        anyhow::ensure!(
            role.as_deref().is_none_or(|r| r == "admin"),
            "reserved admin username belongs to an ordinary account"
        );
        Ok(role.is_some())
    }
    pub fn bootstrap_admin_from_file(&self, path: &Path) -> Result<()> {
        if self.has_admin()? {
            return Ok(());
        }
        let password = private_password(path)?;
        anyhow::ensure!(
            crate::auth::valid_account_password(ADMIN_USERNAME, &password),
            "invalid administrator password"
        );
        let encoded = crate::auth::hash_password_sync(&password)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let role: Option<String> = tx
            .query_row(
                "SELECT role FROM accounts WHERE username=?1",
                [ADMIN_USERNAME],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(role) = role {
            anyhow::ensure!(
                role == "admin",
                "reserved admin username belongs to an ordinary account"
            );
        } else {
            let id = Uuid::new_v4().to_string();
            tx.execute("INSERT INTO accounts(id,username,password,role,must_change_password) VALUES(?1,?2,?3,'admin',1)", params![id,ADMIN_USERNAME,encoded])?;
            audit(&tx, "admin_bootstrapped", Some(&id), None)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn recover_admin_from_file(&self, path: &Path) -> Result<()> {
        anyhow::ensure!(self.has_admin()?, "administrator has not been bootstrapped");
        let password = private_password(path)?;
        anyhow::ensure!(
            crate::auth::valid_account_password(ADMIN_USERNAME, &password),
            "invalid administrator password"
        );
        let encoded = crate::auth::hash_password_sync(&password)?;
        let mut c = self.0.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id: String = tx.query_row(
            "SELECT id FROM accounts WHERE username=?1 AND role='admin'",
            [ADMIN_USERNAME],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE accounts SET password=?1,disabled=0,must_change_password=1 WHERE id=?2",
            params![encoded, id],
        )?;
        revoke_account(&tx, &id)?;
        audit(&tx, "admin_recovered", Some(&id), None)?;
        tx.commit()?;
        Ok(())
    }
    pub fn open(dir: &Path) -> Result<Self> {
        secure_files(dir)?;
        // Resolve OS aliases such as macOS /var only after rejecting a linked configured root.
        let dir = std::fs::canonicalize(dir)?;
        let mut c = Connection::open_with_flags(
            dir.join("relay.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            matches!(version, 0..=3),
            "unsupported future relay database schema"
        );
        c.busy_timeout(std::time::Duration::from_secs(2))?;
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        // A second opener may have completed migration while we waited for the lock.
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        anyhow::ensure!(
            matches!(version, 0..=3),
            "unsupported future relay database schema"
        );
        if version == 0 {
            tx.execute_batch("
        CREATE TABLE accounts(id TEXT PRIMARY KEY, username TEXT UNIQUE NOT NULL, password TEXT, disabled INTEGER NOT NULL DEFAULT 0, role TEXT NOT NULL DEFAULT 'user' CHECK(role IN ('user','admin')), must_change_password INTEGER NOT NULL DEFAULT 0, auth_generation INTEGER NOT NULL DEFAULT 1);
        CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), kind TEXT NOT NULL, expires INTEGER NOT NULL);
        CREATE TABLE gateways(id TEXT PRIMARY KEY, account TEXT NOT NULL REFERENCES accounts(id), name TEXT NOT NULL, credential TEXT UNIQUE NOT NULL, auth_generation INTEGER NOT NULL DEFAULT 1, workbench_api_version INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE sessions(id TEXT PRIMARY KEY, hash TEXT UNIQUE NOT NULL, account TEXT NOT NULL REFERENCES accounts(id), gateway TEXT REFERENCES gateways(id) ON DELETE CASCADE, parent TEXT REFERENCES sessions(id) ON DELETE CASCADE, expires INTEGER NOT NULL, auth_generation INTEGER NOT NULL DEFAULT 1, ui_api_version INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS open_codes(hash TEXT PRIMARY KEY, parent TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, gateway TEXT NOT NULL REFERENCES gateways(id) ON DELETE CASCADE, expires INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, event TEXT NOT NULL, account TEXT, gateway TEXT);
        CREATE INDEX IF NOT EXISTS sessions_expires ON sessions(expires);
        CREATE INDEX IF NOT EXISTS sessions_parent ON sessions(parent);
        CREATE INDEX IF NOT EXISTS tokens_expires ON tokens(expires);
        CREATE INDEX IF NOT EXISTS open_codes_expires ON open_codes(expires);
        CREATE INDEX IF NOT EXISTS open_codes_parent ON open_codes(parent);
        ")?;
        } else if version == 1 {
            tx.execute_batch("ALTER TABLE accounts ADD COLUMN role TEXT NOT NULL DEFAULT 'user' CHECK(role IN ('user','admin'));
            ALTER TABLE accounts ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE accounts ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE gateways ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE gateways ADD COLUMN workbench_api_version INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE sessions ADD COLUMN auth_generation INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE sessions ADD COLUMN ui_api_version INTEGER NOT NULL DEFAULT 0;
            DELETE FROM open_codes; DELETE FROM sessions; DELETE FROM tokens;
            UPDATE gateways SET auth_generation=0;")?;
            audit(&tx, "schema2_credentials_revoked", None, None)?;
        }
        if version < 3 {
            migrate_admin_username(&tx)?;
        }
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
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
    #[cfg(test)]
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
    #[cfg(test)]
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
            revoke_account(&tx, &account)?;
            audit(&tx, "disabled", Some(&account), None)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn session(&self, token: &str, gateway: Option<Uuid>) -> Option<Session> {
        let c = self.0.lock().ok()?;
        let (id,account,g,generation,must_change,role) = c.query_row("SELECT s.id,s.account,s.gateway,s.auth_generation,a.must_change_password,a.role FROM sessions s JOIN accounts a ON a.id=s.account WHERE s.hash=?1 AND s.expires>?2 AND a.disabled=0 AND s.auth_generation=a.auth_generation AND (s.parent IS NULL OR EXISTS(SELECT 1 FROM sessions p WHERE p.id=s.parent AND p.account=s.account AND p.gateway IS NULL AND p.auth_generation=a.auth_generation AND p.expires>?2))", params![digest(token),now()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().ok()??;
        Some(Session {
            id: Uuid::parse_str(&id).ok()?,
            account: Uuid::parse_str(&account).ok()?,
            gateway: g.map(|s| Uuid::parse_str(&s)).transpose().ok()?,
            auth_generation: generation,
            must_change_password: must_change,
            role,
        })
        .filter(|s| s.gateway == gateway)
    }
    pub fn active(&self, id: Uuid) -> bool {
        self.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sessions s JOIN accounts a ON a.id=s.account WHERE s.id=?1 AND s.expires>?2 AND a.disabled=0 AND a.must_change_password=0 AND s.auth_generation=a.auth_generation AND (s.gateway IS NULL OR EXISTS(SELECT 1 FROM gateways g WHERE g.id=s.gateway AND g.account=a.id AND g.auth_generation=a.auth_generation)) AND (s.parent IS NULL OR EXISTS(SELECT 1 FROM sessions p WHERE p.id=s.parent AND p.account=s.account AND p.gateway IS NULL AND p.auth_generation=a.auth_generation AND p.expires>?2)))", params![id.to_string(),now()], |r|r.get(0)).unwrap_or(false)
    }
    pub fn gateway_auth(&self, token: &str) -> Option<(Uuid, Uuid)> {
        let c = self.0.lock().ok()?;
        let (g,a) = c.query_row("SELECT g.id,g.account FROM gateways g JOIN accounts a ON a.id=g.account WHERE g.credential=?1 AND a.disabled=0 AND a.must_change_password=0 AND g.auth_generation=a.auth_generation",[digest(token)],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().ok()??;
        Some((Uuid::parse_str(&g).ok()?, Uuid::parse_str(&a).ok()?))
    }
}

/// A versioned identity migration, never an administrator grant. The whole
/// schema upgrade rolls back on a collision, including a schema-1 upgrade.
fn migrate_admin_username(c: &Connection) -> Result<()> {
    let target: Option<String> = c
        .query_row(
            "SELECT role FROM accounts WHERE username=?1",
            [ADMIN_USERNAME],
            |r| r.get(0),
        )
        .optional()?;
    anyhow::ensure!(
        target.as_deref().is_none_or(|role| role == "admin"),
        "administrator username migration conflicts with an ordinary windsyu account"
    );
    let legacy: Option<String> = c
        .query_row(
            "SELECT id FROM accounts WHERE username='admin' AND role='admin'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = legacy {
        anyhow::ensure!(
            target.is_none(),
            "administrator username migration conflicts with an existing windsyu account"
        );
        c.execute(
            "UPDATE accounts SET username=?1 WHERE id=?2",
            params![ADMIN_USERNAME, id],
        )?;
        audit(c, "admin_username_migrated", Some(&id), None)?;
    }
    Ok(())
}

/// Caller holds a write transaction. Keep credential hashes solely as proof for reauthentication.
pub(crate) fn revoke_account(c: &Connection, account: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE accounts SET auth_generation=auth_generation+1 WHERE id=?1",
        [account],
    )?;
    c.execute("DELETE FROM sessions WHERE account=?1", [account])?;
    c.execute("DELETE FROM tokens WHERE account=?1", [account])?;
    Ok(())
}

#[cfg(unix)]
fn private_password(path: &Path) -> Result<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == uid
            && metadata.mode() & 0o077 == 0,
        "password file must be a private, owned regular file"
    );
    let mut bytes = Vec::new();
    file.take(515).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 514, "password file is too large");
    if bytes.ends_with(b"\n") {
        bytes.pop();
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
    }
    anyhow::ensure!(bytes.len() <= 512, "password file is too large");
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("password file is not valid UTF-8"))
}
#[cfg(not(unix))]
fn private_password(_path: &Path) -> Result<String> {
    anyhow::bail!("relay requires a Unix service host")
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

#[cfg(test)]
#[path = "account_store_tests.rs"]
mod account_store_tests;
