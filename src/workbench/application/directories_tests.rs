use super::*;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::TempDir;

fn request(instance: Uuid, path: Option<&Path>) -> BrowseRequest {
    BrowseRequest {
        instance_id: instance,
        path: path.map(|path| path.to_str().unwrap().to_owned()),
        show_hidden: false,
        filter: String::new(),
        cursor: None,
    }
}

fn children(root: &Path, count: usize) {
    for index in 0..count {
        std::fs::create_dir(root.join(format!("project-{index:04}"))).unwrap();
    }
}

#[tokio::test]
async fn browse_metadata_only_with_unicode_hidden_filter_and_symlinks() {
    let home = TempDir::new().unwrap();
    for name in ["项目 一", ".hidden", "Visible", "<script>"] {
        std::fs::create_dir(home.path().join(name)).unwrap();
    }
    std::fs::write(home.path().join("file.txt"), "not a directory").unwrap();
    symlink(home.path().join("项目 一"), home.path().join("link")).unwrap();
    symlink(home.path().join("missing"), home.path().join("broken")).unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let response = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    assert_eq!(
        response.current_path,
        home.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(response.home_path, response.current_path);
    assert!(response.can_select);
    assert!(response.selection_error.is_none());
    assert_eq!(response.entries.len(), 4);
    assert!(
        response
            .entries
            .iter()
            .any(|entry| entry.name == "<script>")
    );
    let link = response
        .entries
        .iter()
        .find(|entry| entry.name == "link")
        .unwrap();
    assert!(link.is_symlink);
    let entered = browser
        .list(request(instance, Some(Path::new(&link.path))), instance)
        .await
        .unwrap();
    assert_eq!(
        entered.current_path,
        home.path()
            .join("项目 一")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(entered.entries.is_empty());
    let mut filtered = request(instance, None);
    filtered.filter = "VIS".into();
    let response = browser.list(filtered, instance).await.unwrap();
    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].name, "Visible");
    let mut hidden = request(instance, None);
    hidden.show_hidden = true;
    hidden.filter = "hidden".into();
    assert_eq!(
        browser.list(hidden, instance).await.unwrap().entries[0].name,
        ".hidden"
    );
}

#[tokio::test]
async fn pagination_is_stable_bounded_and_bound_to_all_request_fields() {
    let home = TempDir::new().unwrap();
    children(home.path(), 205);
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let first = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 100);
    assert!(!first.truncated);
    assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_RESPONSE_BYTES);
    let mut next = request(instance, None);
    next.cursor = first.next_cursor.clone();
    let second = browser.list(next.clone(), instance).await.unwrap();
    assert_eq!(second.entries.len(), 100);
    assert!(first.entries.last().unwrap().name < second.entries[0].name);
    assert_eq!(
        browser.list(next.clone(), instance).await.unwrap().entries[0].name,
        second.entries[0].name
    );
    let mut last = next.clone();
    last.cursor = second.next_cursor;
    let last = browser.list(last, instance).await.unwrap();
    assert_eq!(last.entries.len(), 5);
    assert!(last.next_cursor.is_none());
    let mut changed = next.clone();
    changed.show_hidden = true;
    assert_eq!(
        browser.list(changed, instance).await.unwrap_err(),
        "invalid_directory_cursor"
    );
    let mut changed = next.clone();
    changed.filter = "project".into();
    assert_eq!(
        browser.list(changed, instance).await.unwrap_err(),
        "invalid_directory_cursor"
    );
    let mut changed = next.clone();
    changed.path = Some(home.path().join("project-0000").to_str().unwrap().into());
    assert_eq!(
        browser.list(changed, instance).await.unwrap_err(),
        "invalid_directory_cursor"
    );
    let other_instance = Uuid::new_v4();
    next.instance_id = other_instance;
    assert_eq!(
        browser.list(next, other_instance).await.unwrap_err(),
        "invalid_directory_cursor"
    );
}

#[tokio::test]
async fn long_names_are_paginated_before_response_byte_limit() {
    let home = TempDir::new().unwrap();
    let mut directory = home.path().to_path_buf();
    for _ in 0..3 {
        directory.push("nested".repeat(32));
        std::fs::create_dir(&directory).unwrap();
    }
    for index in 0..105 {
        std::fs::create_dir(directory.join(format!("{index:03}-{}", "name".repeat(48)))).unwrap();
    }
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let mut next = request(instance, Some(&directory));
    let mut found = Vec::new();
    loop {
        let page = browser.list(next.clone(), instance).await.unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_RESPONSE_BYTES);
        assert!(!page.entries.is_empty());
        assert!(page.entries.len() < PAGE_SIZE);
        found.extend(page.entries.into_iter().map(|entry| entry.name));
        next.cursor = page.next_cursor;
        if next.cursor.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 105);
    found.sort();
    found.dedup();
    assert_eq!(found.len(), 105);
}

#[tokio::test]
async fn stale_instances_invalid_paths_filters_and_cursors_are_rejected() {
    let home = TempDir::new().unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    assert_eq!(
        browser
            .list(request(Uuid::new_v4(), None), instance)
            .await
            .unwrap_err(),
        "stale_instance"
    );
    assert_eq!(
        browser
            .list(request(Uuid::nil(), None), Uuid::nil())
            .await
            .unwrap_err(),
        "stale_instance"
    );
    for path in [
        "".to_owned(),
        "/tmp/\n".into(),
        "x".repeat(4097),
        format!("/{}", "x/".repeat(65)),
    ] {
        let mut invalid = request(instance, None);
        invalid.path = Some(path);
        assert_eq!(
            browser.list(invalid, instance).await.unwrap_err(),
            "invalid_directory_path"
        );
    }
    let mut relative = request(instance, None);
    relative.path = Some("relative".into());
    assert_eq!(
        browser.list(relative, instance).await.unwrap_err(),
        "absolute_path_required"
    );
    for filter in ["x".repeat(257), "bad\n".into()] {
        let mut invalid = request(instance, None);
        invalid.filter = filter;
        assert_eq!(
            browser.list(invalid, instance).await.unwrap_err(),
            "invalid_directory_filter"
        );
    }
    for cursor in [
        "garbage".into(),
        format!("{instance}:0"),
        format!("{instance}:999999"),
        format!("{instance}:01"),
        "x".repeat(100),
    ] {
        let mut invalid = request(instance, None);
        invalid.cursor = Some(cursor);
        assert_eq!(
            browser.list(invalid, instance).await.unwrap_err(),
            "invalid_directory_cursor"
        );
    }
    let mut expired = request(instance, None);
    expired.cursor = Some(format!("{}:100", Uuid::new_v4()));
    assert_eq!(
        browser.list(expired, instance).await.unwrap_err(),
        "directory_cursor_expired"
    );
    let json = serde_json::json!({"instanceId":instance,"unexpected":true});
    assert!(serde_json::from_value::<BrowseRequest>(json).is_err());
}

#[tokio::test]
async fn changed_or_replaced_directory_never_continues_cached_listing() {
    let home = TempDir::new().unwrap();
    let project = home.path().join("project");
    std::fs::create_dir(&project).unwrap();
    children(&project, 105);
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let first = browser
        .list(request(instance, Some(&project)), instance)
        .await
        .unwrap();
    let mut next = request(instance, Some(&project));
    next.cursor = first.next_cursor;
    std::fs::create_dir(project.join("added")).unwrap();
    assert_eq!(
        browser.list(next, instance).await.unwrap_err(),
        "directory_changed"
    );
    let first = browser
        .list(request(instance, Some(&project)), instance)
        .await
        .unwrap();
    let mut next = request(instance, Some(&project));
    next.cursor = first.next_cursor;
    std::fs::rename(&project, home.path().join("original")).unwrap();
    std::fs::create_dir(&project).unwrap();
    assert_eq!(
        browser.list(next, instance).await.unwrap_err(),
        "directory_changed"
    );
}

#[tokio::test]
async fn scans_are_finite_and_unsupported_names_report_incomplete() {
    let home = TempDir::new().unwrap();
    for index in 0..MAX_SCANNED + 1 {
        std::fs::write(home.path().join(format!("file-{index}")), "").unwrap();
    }
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let response = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    assert!(response.truncated);
    assert!(response.entries.is_empty());
    let names = TempDir::new().unwrap();
    std::fs::create_dir(names.path().join("safe")).unwrap();
    std::fs::create_dir(names.path().join("bad\nname")).unwrap();
    let browser = DirectoryBrowser::new(names.path().into());
    let response = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    assert!(response.truncated);
    assert_eq!(response.entries.len(), 1);
    assert_eq!(response.entries[0].name, "safe");
}

#[test]
fn unsupported_entry_names_are_omitted_without_lossy_substitution() {
    // APFS itself rejects invalid UTF-8 names; inject the actual enumeration
    // boundary value so this rejection is tested on every supported host.
    assert!(entry_name(std::ffi::OsString::from_vec(vec![0xff])).is_none());
    assert!(entry_name("bad\nname".into()).is_none());
    assert_eq!(entry_name("项目 一".into()).as_deref(), Some("项目 一"));
}

#[tokio::test]
async fn snapshot_cache_has_finite_capacity_and_expiration() {
    let home = TempDir::new().unwrap();
    children(home.path(), 105);
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let first = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    for _ in 0..MAX_SNAPSHOTS {
        browser
            .list(request(instance, None), instance)
            .await
            .unwrap();
    }
    assert_eq!(browser.snapshots.lock().unwrap().len(), MAX_SNAPSHOTS);
    let mut evicted = request(instance, None);
    evicted.cursor = first.next_cursor;
    assert_eq!(
        browser.list(evicted, instance).await.unwrap_err(),
        "directory_cursor_expired"
    );
    let latest = browser
        .list(request(instance, None), instance)
        .await
        .unwrap();
    let (id, _) = parse_cursor(latest.next_cursor.as_ref().unwrap()).unwrap();
    {
        let mut snapshots = browser.snapshots.lock().unwrap();
        Arc::get_mut(snapshots.get_mut(&id).unwrap())
            .unwrap()
            .created = Instant::now() - SNAPSHOT_TTL;
    }
    let mut expired = request(instance, None);
    expired.cursor = latest.next_cursor;
    assert_eq!(
        browser.list(expired, instance).await.unwrap_err(),
        "directory_cursor_expired"
    );
}

#[tokio::test]
async fn concurrent_work_has_no_waiting_queue() {
    let home = TempDir::new().unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let first = browser.scans.clone().try_acquire_owned().unwrap();
    let second = browser.scans.clone().try_acquire_owned().unwrap();
    assert_eq!(
        browser
            .list(request(instance, None), instance)
            .await
            .unwrap_err(),
        "directory_busy"
    );
    drop(first);
    assert!(
        browser
            .list(request(instance, None), instance)
            .await
            .is_ok()
    );
    drop(second);
}

#[tokio::test]
async fn timed_out_and_cancelled_scans_keep_their_permits_until_io_finishes() {
    let home = TempDir::new().unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let (release_first, wait_first) = std::sync::mpsc::channel();
    let (release_second, wait_second) = std::sync::mpsc::channel();
    let first = browser.clone();
    let first = tokio::spawn(async move {
        first
            .blocking(move || {
                let _ = wait_first.recv();
                Err("released")
            })
            .await
    });
    let second = browser.clone();
    let second = tokio::spawn(async move {
        second
            .blocking(move || {
                let _ = wait_second.recv();
                Err("released")
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while browser.scans.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    second.abort();
    assert_eq!(first.await.unwrap().unwrap_err(), "directory_busy");
    assert_eq!(browser.scans.available_permits(), 0);
    let instance = Uuid::new_v4();
    assert_eq!(
        browser
            .list(request(instance, None), instance)
            .await
            .unwrap_err(),
        "directory_busy"
    );
    release_first.send(()).unwrap();
    release_second.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while browser.scans.available_permits() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        browser
            .list(request(instance, None), instance)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn inaccessible_and_missing_paths_have_safe_errors() {
    let home = TempDir::new().unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let missing = home.path().join("missing");
    assert_eq!(
        browser
            .list(request(instance, Some(&missing)), instance)
            .await
            .unwrap_err(),
        "directory_unavailable"
    );
    let file = home.path().join("file");
    std::fs::write(&file, "private body").unwrap();
    assert_eq!(
        browser
            .list(request(instance, Some(&file)), instance)
            .await
            .unwrap_err(),
        "directory_unavailable"
    );
    let forbidden = home.path().join("forbidden");
    std::fs::create_dir(&forbidden).unwrap();
    std::fs::set_permissions(&forbidden, std::fs::Permissions::from_mode(0o000)).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(
            browser
                .list(request(instance, Some(&forbidden)), instance)
                .await
                .unwrap_err(),
            "directory_forbidden"
        );
    }
    std::fs::set_permissions(&forbidden, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn browsing_readable_system_directory_preserves_launch_selection_restriction() {
    let home = TempDir::new().unwrap();
    let browser = DirectoryBrowser::new(home.path().into());
    let instance = Uuid::new_v4();
    let response = browser
        .list(request(instance, Some(Path::new("/"))), instance)
        .await
        .unwrap();
    assert_eq!(response.current_path, "/");
    assert!(response.parent_path.is_none());
    let allowed = DirectoryGuard::open(Path::new("/")).is_ok();
    assert_eq!(response.can_select, allowed);
    assert_eq!(response.selection_error.is_some(), !allowed);
}
