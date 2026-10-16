use std::{fs, io, path::Path, time::SystemTime};

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
    let web = Path::new("../../web");
    let inputs = [
        "index.html",
        "workbench.html",
        "package.json",
        "package-lock.json",
        "tsconfig.json",
        "vite.config.ts",
        "src",
    ];
    let newest = inputs
        .iter()
        .map(|input| {
            let path = web.join(input);
            println!("cargo:rerun-if-changed={}", path.display());
            newest_modified(&path)
                .unwrap_or_else(|error| panic!("inspect {}: {error}", path.display()))
        })
        .max()
        .expect("frontend inputs");
    for entry in ["index.html", "workbench.html"] {
        let path = web.join("dist").join(entry);
        let built = newest_modified(&path).unwrap_or_else(|_| panic!("{} missing; run `npm ci --prefix web` and `npm run build --prefix web` before Cargo", path.display()));
        assert!(
            built >= newest,
            "web/dist is stale; run `npm run build --prefix web` before Cargo"
        );
    }
    println!("cargo:rerun-if-changed=../../web/dist");
}
