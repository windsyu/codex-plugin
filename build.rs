use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

fn newest_modified(path: &Path) -> io::Result<SystemTime> {
    let metadata = fs::metadata(path)?;
    let mut newest = metadata.modified()?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            newest = newest.max(newest_modified(&entry?.path())?);
        }
    }
    Ok(newest)
}

fn main() {
    let index = Path::new("web/dist/index.html");
    if !index.exists() {
        panic!(
            "web/dist/index.html is missing; run `cd web && npm install && npm run build` before building the Rust binary"
        );
    }

    let web_inputs = [
        Path::new("web/index.html"),
        Path::new("web/package.json"),
        Path::new("web/package-lock.json"),
        Path::new("web/tsconfig.json"),
        Path::new("web/vite.config.ts"),
        Path::new("web/src"),
    ];
    let dist_modified = newest_modified(index).expect("inspect web/dist/index.html timestamp");
    let inputs_modified = web_inputs
        .iter()
        .map(|path| {
            newest_modified(path)
                .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()))
        })
        .max()
        .expect("web input list is non-empty");
    if inputs_modified > dist_modified {
        panic!(
            "web/dist is older than the Viewer source; run `npm run build --prefix web` before building the Rust binary"
        );
    }

    for path in web_inputs {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rerun-if-changed=web/dist");
    build_folder_picker();
}

fn build_folder_picker() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let source = "src/workbench/application/native_picker.m";
    let plist = "src/workbench/application/native_picker.plist";
    println!("cargo:rerun-if-changed={source}");
    println!("cargo:rerun-if-changed={plist}");
    let output =
        Path::new(&std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR")).join("codex-folder-picker");
    let compiler = cc::Build::new().file(source).get_compiler();
    let status = compiler
        .to_command()
        .args(["-fobjc-arc", source, "-framework", "AppKit"])
        .args(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT"])
        .args(["-Xlinker", "__info_plist", "-Xlinker", plist])
        .arg("-o")
        .arg(output)
        .status()
        .expect("build macOS folder picker using the Apple SDK compiler");
    assert!(status.success(), "macOS folder picker compilation failed");
}
