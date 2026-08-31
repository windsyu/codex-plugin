//! SQLite schema assets and version ordering.

pub(super) const COMPATIBILITY_MANIFEST: &str =
    include_str!("../../compatibility/codex-41ece455.json");

pub(super) const MIGRATION_1: &str = include_str!("../../migrations/0001_initial.sql");
pub(super) const MIGRATION_2: &str = include_str!("../../migrations/0002_fts_trigram.sql");
pub(super) const MIGRATION_3: &str = include_str!("../../migrations/0003_retention.sql");
pub(super) const MIGRATION_4: &str = include_str!("../../migrations/0004_live_sources.sql");
pub(super) const MIGRATION_5: &str = include_str!("../../migrations/0005_blobs.sql");
pub(super) const MIGRATION_6: &str = include_str!("../../migrations/0006_thread_metadata.sql");
pub(super) const MIGRATION_7: &str = include_str!("../../migrations/0007_local_purge.sql");
pub(super) const MIGRATION_8: &str = include_str!("../../migrations/0008_search_lookup_index.sql");
pub(super) const MIGRATION_9: &str =
    include_str!("../../migrations/0009_unknown_rollout_status.sql");
pub(super) const MIGRATION_10: &str = include_str!("../../migrations/0010_completeness_v2.sql");
pub(super) const MIGRATION_11: &str = include_str!("../../migrations/0011_writer_conflicts.sql");
pub(super) const MIGRATION_12: &str = include_str!("../../migrations/0012_context_project.sql");
pub(super) const MIGRATION_13: &str = include_str!("../../migrations/0013_projectless_threads.sql");
pub(super) const MIGRATION_14: &str = include_str!("../../migrations/0014_gateway_commands.sql");
pub(super) const MIGRATION_15: &str = include_str!("../../migrations/0015_thread_goals.sql");

pub const LATEST_SCHEMA_VERSION: i64 = 15;

pub(super) const fn migrations() -> [(i64, &'static str); LATEST_SCHEMA_VERSION as usize] {
    [
        (1, MIGRATION_1),
        (2, MIGRATION_2),
        (3, MIGRATION_3),
        (4, MIGRATION_4),
        (5, MIGRATION_5),
        (6, MIGRATION_6),
        (7, MIGRATION_7),
        (8, MIGRATION_8),
        (9, MIGRATION_9),
        (10, MIGRATION_10),
        (11, MIGRATION_11),
        (12, MIGRATION_12),
        (13, MIGRATION_13),
        (14, MIGRATION_14),
        (15, MIGRATION_15),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_manifest_matches_release_version_and_schema() {
        let manifest: serde_json::Value =
            serde_json::from_str(COMPATIBILITY_MANIFEST).expect("valid compatibility manifest");
        assert_eq!(
            manifest["observerVersion"].as_str(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            manifest["observerSchemaVersion"].as_i64(),
            Some(LATEST_SCHEMA_VERSION)
        );
    }
}
