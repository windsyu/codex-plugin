use std::path::Path;

/// Stable, source-independent project key derived from a thread's current
/// working directory. The goal is to mirror Codex's cwd-based grouping rather
/// than introducing a separate project_id concept.
pub fn project_key(cwd: &str) -> String {
    let input = cwd.trim();
    if input.is_empty() {
        return "unknown".to_string();
    }

    let normalized = input.replace('\\', "/");
    let normalized = normalized
        .strip_prefix("file://")
        .or_else(|| normalized.strip_prefix("file:"))
        .unwrap_or(&normalized);
    let absolute = normalized.starts_with('/');

    let mut segments: Vec<String> = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.is_empty() {
                    if !absolute {
                        segments.push("..".to_string());
                    }
                } else if segments.last().is_some_and(|value| value == "..") {
                    segments.push("..".to_string());
                } else {
                    segments.pop();
                }
            }
            value => segments.push(value.to_string()),
        }
    }

    if absolute {
        format!("/{}", segments.join("/"))
    } else if segments.is_empty() {
        "unknown".to_string()
    } else {
        segments.join("/")
    }
}

pub fn project_name(key: &str) -> String {
    if key == "/" {
        return "/".to_string();
    }
    Path::new(key)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_file_urls_and_separators() {
        assert_eq!(
            project_key("file:///demo/local-observer"),
            "/demo/local-observer"
        );
        assert_eq!(
            project_key("file:/demo/local-observer"),
            "/demo/local-observer"
        );
        assert_eq!(
            project_key("/demo//local-observer/"),
            "/demo/local-observer"
        );
        assert_eq!(project_key("C:\\demo\\project"), "C:/demo/project");
    }

    #[test]
    fn resolves_dot_and_dot_dot_lexically() {
        assert_eq!(
            project_key("/demo/./local-observer"),
            "/demo/local-observer"
        );
        assert_eq!(
            project_key("/demo/child/../local-observer"),
            "/demo/local-observer"
        );
        assert_eq!(project_key("../outside/project"), "../outside/project");
    }

    #[test]
    fn names_and_fallbacks_are_stable() {
        assert_eq!(project_name("/demo/local-observer"), "local-observer");
        assert_eq!(project_name("/"), "/");
        assert_eq!(project_name("unknown"), "unknown");
        assert_eq!(project_name("/trailing/"), "trailing");
    }
}
