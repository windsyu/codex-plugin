//! Shared static UI only; no CLI, filesystem or account runtime dependency.
#[derive(rust_embed::RustEmbed)]
#[folder = "../../web/dist/"]
pub struct WorkbenchAssets;

/// Only canonical compiled scripts and styles are externally served as assets.
pub fn content_type(path: &str) -> Option<&'static str> {
    if !path.starts_with("assets/")
        || path.contains(['%', '\\', '?', '#'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    if path.ends_with(".js") {
        Some("text/javascript; charset=utf-8")
    } else if path.ends_with(".css") {
        Some("text/css; charset=utf-8")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_ui_and_canonical_asset_paths() {
        assert!(WorkbenchAssets::get("workbench.html").is_some());
        assert!(content_type("assets/workbench-123.js").is_some());
        for path in [
            "assets/../x.js",
            "assets/%2e.js",
            "assets//x.js",
            "/assets/x.js",
            "assets/x.svg",
            "assets/x.js?x",
        ] {
            assert_eq!(content_type(path), None, "{path}");
        }
    }
}
