use std::path::Path;

fn main() {
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
