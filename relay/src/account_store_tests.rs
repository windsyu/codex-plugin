use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

fn password_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("bootstrap-secret");
    std::fs::write(&path, "synthetic cloud pear 529\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn schema3_renames_only_the_legacy_administrator_without_changing_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = Uuid::new_v4().to_string();
    let gateway = Uuid::new_v4().to_string();
    {
        let c = store.0.lock().unwrap();
        c.execute("INSERT INTO accounts(id,username,password,role,auth_generation) VALUES(?1,'admin','unchanged-password-hash','admin',7)", [&account]).unwrap();
        c.execute("INSERT INTO gateways(id,account,name,credential,auth_generation) VALUES(?1,?2,'fixture',?3,7)",params![gateway,account,digest("machine")]).unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires,auth_generation) VALUES(?1,?2,?3,?4,7)",
            params![
                Uuid::new_v4().to_string(),
                digest("browser"),
                account,
                now() + 3600
            ],
        )
        .unwrap();
        c.execute_batch("PRAGMA user_version=2").unwrap();
    }
    drop(store);
    for _ in 0..2 {
        let store = Store::open(dir.path()).unwrap();
        assert!(store.has_admin().unwrap());
        assert_eq!(
            store.session("browser", None).unwrap().account.to_string(),
            account
        );
        assert_eq!(
            store.gateway_auth("machine").unwrap().0.to_string(),
            gateway
        );
        // Existing admin never requires re-reading a deleted bootstrap file.
        store
            .bootstrap_admin_from_file(&dir.path().join("absent"))
            .unwrap();
        let c = store.0.lock().unwrap();
        let identity: (String,String,i64,bool,bool) = c.query_row("SELECT username,password,auth_generation,disabled,must_change_password FROM accounts WHERE id=?1",[&account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap();
        assert_eq!(
            identity,
            (
                "windsyu".into(),
                "unchanged-password-hash".into(),
                7,
                false,
                false
            )
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM audit WHERE event='admin_username_migrated' AND account=?1",
                [&account],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
}

#[test]
fn schema3_conflicts_roll_back_and_ordinary_admin_is_not_promoted() {
    for role in ["user", "admin"] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        {
            let c = store.0.lock().unwrap();
            c.execute(
                "INSERT INTO accounts(id,username,role) VALUES(?1,'admin','admin')",
                [Uuid::new_v4().to_string()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO accounts(id,username,role) VALUES(?1,'windsyu',?2)",
                params![Uuid::new_v4().to_string(), role],
            )
            .unwrap();
            c.execute_batch("PRAGMA user_version=2").unwrap();
        }
        drop(store);
        assert!(
            Store::open(dir.path())
                .err()
                .unwrap()
                .to_string()
                .contains("conflicts")
        );
        let c = Connection::open(dir.path().join("relay.sqlite3")).unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM accounts WHERE username='admin' AND role='admin'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM audit WHERE event='admin_username_migrated'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store.0.lock().unwrap().execute_batch("INSERT INTO accounts(id,username,role) VALUES('ordinary','admin','user'); PRAGMA user_version=2").unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert!(!store.has_admin().unwrap());
    store
        .bootstrap_admin_from_file(&password_file(dir.path()))
        .unwrap();
    let c = store.0.lock().unwrap();
    assert_eq!(
        c.query_row("SELECT role FROM accounts WHERE id='ordinary'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "user"
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM accounts WHERE username='windsyu' AND role='admin'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn schema1_to_schema3_collision_rolls_back_columns_and_credential_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let gateway = Uuid::new_v4().to_string();
    let parent = Uuid::new_v4().to_string();
    {
        let c = Connection::open(dir.path().join("relay.sqlite3")).unwrap();
        c.execute_batch("CREATE TABLE accounts(id TEXT PRIMARY KEY,username TEXT UNIQUE NOT NULL,password TEXT,disabled INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE gateways(id TEXT PRIMARY KEY,account TEXT NOT NULL REFERENCES accounts(id),name TEXT NOT NULL,credential TEXT UNIQUE NOT NULL);
        CREATE TABLE sessions(id TEXT PRIMARY KEY,hash TEXT UNIQUE NOT NULL,account TEXT NOT NULL REFERENCES accounts(id),gateway TEXT REFERENCES gateways(id) ON DELETE CASCADE,parent TEXT REFERENCES sessions(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
        CREATE TABLE tokens(hash TEXT PRIMARY KEY,account TEXT NOT NULL REFERENCES accounts(id),kind TEXT NOT NULL,expires INTEGER NOT NULL);
        CREATE TABLE open_codes(hash TEXT PRIMARY KEY,parent TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,gateway TEXT NOT NULL REFERENCES gateways(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
        CREATE TABLE audit(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,event TEXT NOT NULL,account TEXT,gateway TEXT);
        PRAGMA user_version=1;").unwrap();
        c.execute(
            "INSERT INTO accounts(id,username,password) VALUES(?1,'windsyu','unchanged-hash')",
            [&account],
        )
        .unwrap();
        c.execute(
            "INSERT INTO gateways VALUES(?1,?2,'fixture','credential-hash')",
            params![gateway, account],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,'browser-hash',?2,?3)",
            params![parent, account, now() + 3600],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tokens VALUES('invite-hash',?1,'invite',?2)",
            params![account, now() + 3600],
        )
        .unwrap();
        c.execute(
            "INSERT INTO open_codes VALUES('open-hash',?1,?2,?3)",
            params![parent, gateway, now() + 3600],
        )
        .unwrap();
    }
    assert!(
        Store::open(dir.path())
            .err()
            .unwrap()
            .to_string()
            .contains("conflicts")
    );
    let c = Connection::open(dir.path().join("relay.sqlite3")).unwrap();
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM pragma_table_info('accounts') WHERE name='role'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM pragma_table_info('gateways') WHERE name='auth_generation'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    for table in ["accounts", "gateways", "sessions", "tokens", "open_codes"] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1,
            "{table}"
        );
    }
    assert_eq!(
        c.query_row("SELECT password FROM accounts", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "unchanged-hash"
    );
    assert_eq!(
        c.query_row("SELECT count(*) FROM audit", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn bootstrap_is_private_idempotent_and_recovery_revokes_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let path = password_file(dir.path());
    let original = std::fs::read(&path).unwrap();
    store.bootstrap_admin_from_file(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(store.has_admin().unwrap());
    let (id, password, generation): (String, String, i64) = store
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT id,password,auth_generation FROM accounts WHERE username='windsyu'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(password.starts_with("$argon2id$"));
    assert!(!password.contains("synthetic"));
    store
        .bootstrap_admin_from_file(&dir.path().join("missing-file"))
        .unwrap();
    let token = secret();
    let credential = secret();
    let gateway = Uuid::new_v4();
    {
        let c = store.0.lock().unwrap();
        c.execute(
            "UPDATE accounts SET must_change_password=0 WHERE id=?1",
            [&id],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![Uuid::new_v4().to_string(), digest(&token), id, now() + 60],
        )
        .unwrap();
        c.execute(
            "INSERT INTO gateways(id,account,name,credential) VALUES(?1,?2,'fixture',?3)",
            params![gateway.to_string(), id, digest(&credential)],
        )
        .unwrap();
    }
    assert!(store.session(&token, None).is_some());
    assert!(store.gateway_auth(&credential).is_some());
    store.recover_admin_from_file(&path).unwrap();
    assert!(store.session(&token, None).is_none());
    assert!(store.gateway_auth(&credential).is_none());
    let c = store.0.lock().unwrap();
    assert_eq!(
        c.query_row(
            "SELECT auth_generation FROM accounts WHERE id=?1",
            [&id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        generation + 1
    );
    assert_eq!(
        c.query_row(
            "SELECT credential FROM gateways WHERE id=?1",
            [gateway.to_string()],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        digest(&credential)
    );
}

#[test]
fn bootstrap_rejects_unsafe_secret_and_reserved_ordinary_account() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let path = password_file(dir.path());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.bootstrap_admin_from_file(&path).is_err());
    assert!(!store.has_admin().unwrap());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, vec![b'a'; 515]).unwrap();
    assert!(store.bootstrap_admin_from_file(&path).is_err());
    std::fs::write(&path, "synthetic cloud pear 529\n").unwrap();
    let linked = dir.path().join("linked");
    symlink(&path, &linked).unwrap();
    assert!(store.bootstrap_admin_from_file(&linked).is_err());
    std::fs::hard_link(&path, dir.path().join("hard-link")).unwrap();
    assert!(store.bootstrap_admin_from_file(&path).is_err());
    store
        .0
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO accounts(id,username) VALUES(?1,'windsyu')",
            [Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(store.has_admin().is_err());
    assert!(store.bootstrap_admin_from_file(&path).is_err());
}

#[test]
fn schema1_migration_preserves_ownership_but_revokes_every_credential() {
    let dir = tempfile::tempdir().unwrap();
    let account = Uuid::new_v4().to_string();
    let gateway = Uuid::new_v4().to_string();
    let session = Uuid::new_v4().to_string();
    {
        let c = Connection::open(dir.path().join("relay.sqlite3")).unwrap();
        c.execute_batch("CREATE TABLE accounts(id TEXT PRIMARY KEY,username TEXT UNIQUE NOT NULL,password TEXT,disabled INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE gateways(id TEXT PRIMARY KEY,account TEXT NOT NULL REFERENCES accounts(id),name TEXT NOT NULL,credential TEXT UNIQUE NOT NULL);
        CREATE TABLE sessions(id TEXT PRIMARY KEY,hash TEXT UNIQUE NOT NULL,account TEXT NOT NULL REFERENCES accounts(id),gateway TEXT REFERENCES gateways(id) ON DELETE CASCADE,parent TEXT REFERENCES sessions(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
        CREATE TABLE tokens(hash TEXT PRIMARY KEY,account TEXT NOT NULL REFERENCES accounts(id),kind TEXT NOT NULL,expires INTEGER NOT NULL);
        CREATE TABLE open_codes(hash TEXT PRIMARY KEY,parent TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,gateway TEXT NOT NULL REFERENCES gateways(id) ON DELETE CASCADE,expires INTEGER NOT NULL);
        CREATE TABLE audit(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,event TEXT NOT NULL,account TEXT,gateway TEXT);
        PRAGMA user_version=1;").unwrap();
        c.execute(
            "INSERT INTO accounts(id,username) VALUES(?1,'alice')",
            [&account],
        )
        .unwrap();
        c.execute(
            "INSERT INTO gateways VALUES(?1,?2,'fixture',?3)",
            params![gateway, account, digest("machine")],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![session, digest("browser"), account, now() + 60],
        )
        .unwrap();
        c.execute(
            "INSERT INTO tokens VALUES('invite',?1,'invite',?2)",
            params![account, now() + 60],
        )
        .unwrap();
        c.execute(
            "INSERT INTO open_codes VALUES('open',?1,?2,?3)",
            params![session, gateway, now() + 60],
        )
        .unwrap();
    }
    for _ in 0..2 {
        let store = Store::open(dir.path()).unwrap();
        assert!(store.session("browser", None).is_none());
        assert!(store.gateway_auth("machine").is_none());
        let c = store.0.lock().unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            c.query_row(
                "SELECT account FROM gateways WHERE id=?1",
                [&gateway],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            account
        );
        assert_eq!(c.query_row("SELECT (SELECT count(*) FROM sessions)+(SELECT count(*) FROM tokens)+(SELECT count(*) FROM open_codes)",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
}

#[test]
fn migration_rolls_back_on_invalid_schema_and_rejects_future_versions() {
    let dir = tempfile::tempdir().unwrap();
    let c = Connection::open(dir.path().join("relay.sqlite3")).unwrap();
    c.execute_batch(
        "CREATE TABLE accounts(id TEXT PRIMARY KEY,username TEXT); PRAGMA user_version=1;",
    )
    .unwrap();
    assert!(Store::open(dir.path()).is_err());
    let columns: i64 = c
        .query_row(
            "SELECT count(*) FROM pragma_table_info('accounts') WHERE name='role'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(columns, 0);
    assert_eq!(
        c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    c.execute_batch("PRAGMA user_version=4;").unwrap();
    assert!(Store::open(dir.path()).is_err());
}

#[test]
fn forced_password_and_stale_parent_cannot_authorize_workbench() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let account = Uuid::new_v4();
    let gateway = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    {
        let c = store.0.lock().unwrap();
        c.execute(
            "INSERT INTO accounts(id,username,must_change_password) VALUES(?1,'alice',1)",
            [account.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO gateways(id,account,name,credential) VALUES(?1,?2,'fixture',?3)",
            params![gateway.to_string(), account.to_string(), digest("machine")],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions(id,hash,account,expires) VALUES(?1,?2,?3,?4)",
            params![
                parent.to_string(),
                digest("portal"),
                account.to_string(),
                now() + 60
            ],
        )
        .unwrap();
        c.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",params![child.to_string(),digest("browser"),account.to_string(),gateway.to_string(),parent.to_string(),now()+60]).unwrap();
    }
    assert!(store.session("portal", None).unwrap().must_change_password);
    assert!(!store.active(child));
    assert!(store.gateway_auth("machine").is_none());
    store
        .0
        .lock()
        .unwrap()
        .execute("UPDATE accounts SET must_change_password=0", [])
        .unwrap();
    assert!(store.active(child));
    assert!(store.gateway_auth("machine").is_some());
    store
        .0
        .lock()
        .unwrap()
        .execute(
            "UPDATE sessions SET auth_generation=0 WHERE id=?1",
            [parent.to_string()],
        )
        .unwrap();
    assert!(!store.active(child));
    assert!(store.session("browser", Some(gateway)).is_none());
}
