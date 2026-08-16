use std::path::Path;

fn main() {
    let index = Path::new("web/dist/index.html");
    if !index.exists() {
        panic!(
            "web/dist/index.html is missing; run `cd web && npm install && npm run build` before building the Rust binary"
        );
    }
    println!("cargo:rerun-if-changed=web/dist");
}
