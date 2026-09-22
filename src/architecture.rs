//! Regression checks for ADR 0009 dependency direction.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).expect("read module directory") {
        let path = entry.expect("read module entry").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

fn assert_avoids(module: &str, forbidden: &[&str]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(module);
    for path in rust_files(&root) {
        let source = fs::read_to_string(&path).expect("read Rust module");
        // Fixture setup may use other layers. Require a compiler-enforced
        // test-only file, rather than exempting files based on their name.
        if source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            == Some("#![cfg(test)]")
        {
            continue;
        }
        let production = source
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&source);
        for dependency in forbidden {
            assert!(
                !production.contains(&format!("crate::{dependency}")),
                "{} must not depend on upper layer {dependency}",
                path.display()
            );
        }
    }
}

#[test]
fn adr_0009_layer_dependencies_are_one_way() {
    assert_avoids(
        "domain",
        &["store", "ingest", "live", "session", "http", "writer"],
    );
    assert_avoids("store", &["ingest", "live", "session", "http", "writer"]);
    assert_avoids("ingest", &["live", "session", "http"]);
    assert_avoids("http", &["ingest", "live"]);
    assert_avoids(
        "terminal",
        &[
            "controller",
            "session",
            "store",
            "writer",
            "workbench",
            "http",
        ],
    );
}

#[test]
fn native_workbench_does_not_import_legacy_runtime_or_ui() {
    assert_avoids(
        "workbench",
        &["controller", "session", "store", "writer", "ingest"],
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for path in rust_files(&root.join("src/workbench")) {
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains("../session/") && !source.contains("../controller/"),
            "{} must not include implementation through a legacy path",
            path.display()
        );
    }
    fn check_web(path: &Path) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                check_web(&path);
            } else if path.extension().is_some_and(|e| e == "ts" || e == "tsx") {
                let source = fs::read_to_string(&path).unwrap();
                assert!(
                    !source.contains("../session/") && !source.contains("../controller/"),
                    "{} must not import a legacy UI module",
                    path.display()
                );
            }
        }
    }
    check_web(&root.join("web/src/workbench"));
    check_web(&root.join("web/src/terminal"));
}

#[test]
fn retired_control_modules_and_startup_hooks_cannot_return() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for retired in [
        "src/session",
        "src/controller",
        "src/live",
        "src/http/session_routes.rs",
        "src/store/session.rs",
        "src/store/gateway.rs",
        "web/src/session",
    ] {
        assert!(
            !root.join(retired).exists(),
            "retired implementation returned: {retired}"
        );
    }
    let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
    let production = main.split("\n#[cfg(test)]\nmod tests").next().unwrap();
    for hook in [
        "OwnedAppServerGuard",
        "SessionKernel",
        "recover_gateway_after_restart",
        "recover_sessions_after_restart",
        "sweep_expired_image_uploads",
    ] {
        assert!(
            !production.contains(hook),
            "retired startup hook returned: {hook}"
        );
    }
}

#[test]
fn adr_0009_required_module_roots_exist() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for (module, files) in [
        (
            "domain",
            &[
                "mod",
                "model",
                "project",
                "redact",
                "classify",
                "identity",
                "normalize",
                "live",
            ][..],
        ),
        (
            "store",
            &[
                "mod",
                "schema",
                "pool",
                "blob",
                "write",
                "query",
                "maintenance",
                "projection",
            ][..],
        ),
        ("ingest", &["mod", "importer", "io", "keys"][..]),
        ("terminal", &["mod"][..]),
        (
            "http",
            &[
                "mod",
                "router",
                "middleware",
                "auth",
                "cursor",
                "query",
                "handlers",
                "stream",
            ][..],
        ),
    ] {
        for file in files {
            assert!(
                src.join(module).join(format!("{file}.rs")).is_file(),
                "missing {module}/{file}.rs"
            );
        }
    }
}
