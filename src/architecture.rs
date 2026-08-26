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
    assert_avoids("domain", &["store", "ingest", "live", "http", "writer"]);
    assert_avoids("store", &["ingest", "live", "http", "writer"]);
    assert_avoids("ingest", &["live", "http"]);
    assert_avoids("live", &["ingest", "http"]);
    assert_avoids("http", &["ingest", "live"]);
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
        ("live", &["mod", "transport"][..]),
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
