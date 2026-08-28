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

/// Returns the inferred project key that durable rollout history can support.
///
/// Codex Desktop gives projectless conversations an isolated working directory
/// under `Documents/Codex/YYYY-MM-DD/<name>`. That cwd is execution context,
/// not a user-visible Codex project, so it must not become a project by itself.
pub fn inferred_project_key(cwd: &str, originator: Option<&str>) -> Option<String> {
    let key = project_key(cwd);
    if key == "unknown"
        || originator.is_some_and(is_codex_desktop_originator)
            && is_codex_projectless_workspace(&key)
    {
        None
    } else {
        Some(key)
    }
}

fn is_codex_desktop_originator(value: &str) -> bool {
    value.eq_ignore_ascii_case("Codex Desktop") || value.eq_ignore_ascii_case("codex_work_desktop")
}

fn is_codex_projectless_workspace(key: &str) -> bool {
    let segments = key
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 4 {
        return false;
    }
    let tail = &segments[segments.len() - 4..];
    tail[0] == "Documents" && tail[1] == "Codex" && is_iso_date(tail[2]) && !tail[3].is_empty()
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
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

    #[test]
    fn codex_desktop_generated_workspaces_are_projectless() {
        assert_eq!(
            inferred_project_key(
                "/Users/demo/Documents/Codex/2026-08-28/generated-name",
                Some("Codex Desktop")
            ),
            None
        );
        assert_eq!(
            inferred_project_key(
                "/Users/demo/Documents/Codex/2026-08-28/generated-name",
                Some("codex_work_desktop")
            ),
            None
        );
        assert_eq!(
            inferred_project_key(
                "/Users/demo/Documents/Codex/2026-08-28/generated-name",
                Some("codex-tui")
            ),
            Some("/Users/demo/Documents/Codex/2026-08-28/generated-name".into())
        );
        assert_eq!(
            inferred_project_key("/Users/demo/workspace/real-project", Some("Codex Desktop")),
            Some("/Users/demo/workspace/real-project".into())
        );
        assert_eq!(inferred_project_key("", Some("Codex Desktop")), None);
    }
}
