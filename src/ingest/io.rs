use std::path::Path;

use uuid::Uuid;

pub(super) fn thread_id_from_filename(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".zst")
        .unwrap_or(name)
        .strip_suffix(".jsonl")?;
    for start in (0..stem.len()).rev() {
        let Some(candidate) = stem.get(start..start.saturating_add(36)) else {
            continue;
        };
        if Uuid::parse_str(candidate).is_ok() {
            return Some(candidate.to_string());
        }
    }
    stem.strip_prefix("rollout-").map(str::to_string)
}
