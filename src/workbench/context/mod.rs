//! Request-local semantic estimates, independent of reading previews and usage totals.
// Classification inspired by yetone/magpie, commit
// 1c45645b59e944b0446772a6786f1a31dd410a80 (MIT, Copyright (c) 2026 yetone).
// See THIRD_PARTY_NOTICES.md for the upstream permission notice.

mod analyze;
mod present;

pub(crate) use analyze::analyze;
pub(crate) use present::present;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    System,
    Tools,
    Skills,
    Memory,
    Files,
    Results,
    Chat,
}

impl ContextKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Tools => "tools",
            Self::Skills => "skills",
            Self::Memory => "memory",
            Self::Files => "files",
            Self::Results => "results",
            Self::Chat => "chat",
        }
    }
}

pub(crate) const KINDS: [ContextKind; 7] = [
    ContextKind::System,
    ContextKind::Tools,
    ContextKind::Skills,
    ContextKind::Memory,
    ContextKind::Files,
    ContextKind::Results,
    ContextKind::Chat,
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCoverage {
    pub complete: bool,
    pub issues: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPartSummary {
    pub kind: ContextKind,
    pub content_bytes: u64,
    pub estimated_tokens: u64,
    pub item_count: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSummary {
    pub schema_version: u32,
    pub method_version: u32,
    pub content_bytes: u64,
    pub estimated_tokens: u64,
    pub parts: Vec<ContextPartSummary>,
    pub coverage: ContextCoverage,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextItem {
    pub id: String,
    pub kind: ContextKind,
    pub name: String,
    pub tag: String,
    pub occurrences: u32,
    pub content_bytes: u64,
    pub estimated_tokens: u64,
    pub references: Vec<String>,
    pub references_partial: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextAnalysis {
    pub summary: ContextSummary,
    pub items: Vec<ContextItem>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentedPart {
    #[serde(flatten)]
    pub part: ContextPartSummary,
    pub tokens: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentedItem {
    #[serde(flatten)]
    pub item: ContextItem,
    pub tokens: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPresentation {
    pub schema_version: u32,
    pub content_bytes: u64,
    pub estimated_tokens: u64,
    pub tokens: u64,
    pub calibrated: bool,
    pub reported_input_tokens: Option<u64>,
    pub coverage: ContextCoverage,
    pub parts: Vec<PresentedPart>,
    pub items: Vec<PresentedItem>,
}

#[cfg(test)]
mod tests;
