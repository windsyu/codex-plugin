//! Shared live/history context projection and source-scoped pagination.
use super::{DetailDocument, DetailEntry, DetailSource};
use crate::workbench::context::{ContextPresentation, present};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub(crate) const PAGE_BYTES: usize = 128 * 1024;

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailSelection {
    pub document: Option<String>,
    pub category: Option<String>,
    pub item: Option<String>,
}
impl DetailSelection {
    pub(crate) fn valid(&self) -> bool {
        (self.document.is_some() || (self.category.is_none() && self.item.is_none()))
            && self.document.as_ref().is_none_or(|s| s.len() <= 100)
            && self.item.as_ref().is_none_or(|s| s.len() <= 128)
            && self.category.as_deref().is_none_or(|kind| {
                matches!(
                    kind,
                    "system" | "tools" | "skills" | "memory" | "files" | "results" | "chat"
                )
            })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub source: DetailSource,
    pub capture_seq: u64,
    #[serde(flatten)]
    pub entry: DetailEntry,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSource {
    pub id: String,
    pub client_request_index: Option<u64>,
    pub capture_seq: u64,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextMetadata {
    pub context_sources: Vec<ContextSource>,
    pub selected_context: Option<String>,
    pub context_summary: Option<ContextPresentation>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestDetailsPage {
    pub run_epoch: Uuid,
    pub request_id: Uuid,
    pub revision: u64,
    pub availability: &'static str,
    pub request_captured: bool,
    pub response_captured: bool,
    pub truncated: bool,
    pub omitted: bool,
    pub conflict: bool,
    pub total_entries: usize,
    pub capture_issues: Vec<String>,
    pub entries: Vec<ContextEntry>,
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub context: Option<ContextMetadata>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DetailError {
    NotFound,
    Evicted,
    InvalidCursor,
    StaleCursor,
}

pub(crate) struct DocumentRef<'a> {
    pub document: &'a DetailDocument,
    pub capture_seq: u64,
}

pub(crate) struct DetailRead<'a> {
    pub epoch: Uuid,
    pub request_id: Uuid,
    pub revision: u64,
    pub before: Option<u64>,
    pub documents: Vec<DocumentRef<'a>>,
    pub responses: &'a [Value],
    pub cursor: Option<&'a str>,
    pub selection: &'a DetailSelection,
    pub truncated: bool,
    pub omitted: bool,
    pub conflict: bool,
    pub capture_issues: Vec<String>,
    pub sources_partial: bool,
}

fn source_id(request: Uuid, revision: u64, index: usize) -> String {
    format!("{request}.{revision}.{index}")
}

fn selected_index(read: &DetailRead<'_>) -> Result<Option<usize>, DetailError> {
    if let Some(selector) = &read.selection.document {
        let fields: Vec<_> = selector.split('.').collect();
        if fields.len() != 3 || fields[0].parse::<Uuid>().ok() != Some(read.request_id) {
            return Err(DetailError::InvalidCursor);
        }
        let revision = fields[1]
            .parse::<u64>()
            .map_err(|_| DetailError::InvalidCursor)?;
        if revision != read.revision {
            return Err(DetailError::StaleCursor);
        }
        let index = fields[2]
            .parse::<usize>()
            .map_err(|_| DetailError::InvalidCursor)?;
        if !read
            .documents
            .get(index)
            .is_some_and(|d| matches!(d.document.source, DetailSource::Request { .. }))
        {
            return Err(DetailError::InvalidCursor);
        }
        Ok(Some(index))
    } else {
        Ok(read
            .documents
            .iter()
            .rposition(|d| matches!(d.document.source, DetailSource::Request { .. })))
    }
}

fn reported_input(read: &DetailRead<'_>, selected: usize) -> Option<u64> {
    if read.conflict || read.sources_partial || !read.capture_issues.is_empty() {
        return None;
    }
    if !matches!(
        read.documents[selected].document.source,
        DetailSource::Request {
            client_request_index: None
        }
    ) {
        return None;
    }
    if read
        .documents
        .iter()
        .filter(|d| matches!(d.document.source, DetailSource::Request { .. }))
        .count()
        != 1
    {
        return None;
    }
    let mut response_documents = read
        .documents
        .iter()
        .filter_map(|d| match &d.document.source {
            DetailSource::Response { response_id } => Some(response_id.as_deref()),
            _ => None,
        });
    let id = response_documents.next()??;
    if response_documents.next().is_some() {
        return None;
    }
    let mut responses = read
        .responses
        .iter()
        .filter(|r| r["requestId"] == read.request_id.to_string());
    let response = responses.next()?;
    if responses.next().is_some()
        || response["responseId"] != id
        || response["status"] != "completed"
        || response["usageConflict"] == true
        || response["usage"]["invalid"] != false
    {
        return None;
    }
    response["usage"]["inputTokens"]
        .as_u64()
        .filter(|n| *n <= 9_007_199_254_740_991)
}

pub(crate) fn read_page(read: DetailRead<'_>) -> Result<RequestDetailsPage, DetailError> {
    if !read.selection.valid() {
        return Err(DetailError::InvalidCursor);
    }
    let selected = selected_index(&read)?;
    let scope =
        serde_json::to_vec(&(read.before, read.selection)).expect("closed details selector");
    let digest = blake3::hash(&scope).to_hex()[..32].to_owned();
    let prefix = format!(
        "{}.{}.{}.{}",
        read.epoch, read.request_id, read.revision, digest
    );
    let offset = if let Some(cursor) = read.cursor {
        let fields: Vec<_> = cursor.split('.').collect();
        if fields.len() != 5
            || fields[0].parse::<Uuid>().ok() != Some(read.epoch)
            || fields[1].parse::<Uuid>().ok() != Some(read.request_id)
        {
            return Err(DetailError::InvalidCursor);
        }
        let revision = fields[2]
            .parse::<u64>()
            .map_err(|_| DetailError::InvalidCursor)?;
        if revision != read.revision {
            return Err(DetailError::StaleCursor);
        }
        if fields[3] != digest {
            return Err(DetailError::InvalidCursor);
        }
        fields[4]
            .parse::<usize>()
            .map_err(|_| DetailError::InvalidCursor)?
    } else {
        0
    };

    let analysis = selected
        .and_then(|index| read.documents[index].document.context_analysis.as_ref())
        .filter(|a| a.summary.schema_version == 1);
    let matching_items = analysis
        .map(|a| {
            a.items
                .iter()
                .filter(|item| {
                    read.selection
                        .category
                        .as_ref()
                        .is_none_or(|k| item.kind.as_str() == k)
                        && read.selection.item.as_ref().is_none_or(|id| item.id == *id)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if read.selection.item.is_some() && matching_items.is_empty() {
        return Err(DetailError::InvalidCursor);
    }
    let filtered = read.selection.category.is_some() || read.selection.item.is_some();
    let mut entries = Vec::new();
    for (index, document) in read.documents.iter().enumerate() {
        if read.selection.document.is_some() && Some(index) != selected {
            continue;
        }
        for entry in &document.document.entries {
            if filtered
                && !matching_items
                    .iter()
                    .any(|item| item.references.contains(&entry.json_pointer))
            {
                continue;
            }
            entries.push((document, entry));
        }
    }
    if offset > entries.len() {
        return Err(DetailError::InvalidCursor);
    }
    let metadata = read.cursor.is_none().then(|| ContextMetadata {
        context_sources: read
            .documents
            .iter()
            .enumerate()
            .filter_map(|(index, d)| match d.document.source {
                DetailSource::Request {
                    client_request_index,
                } => Some(ContextSource {
                    id: source_id(read.request_id, read.revision, index),
                    client_request_index,
                    capture_seq: d.capture_seq,
                    available: d
                        .document
                        .context_analysis
                        .as_ref()
                        .is_some_and(|a| a.summary.schema_version == 1),
                }),
                _ => None,
            })
            .collect(),
        selected_context: selected.map(|index| source_id(read.request_id, read.revision, index)),
        context_summary: analysis
            .map(|a| present(a, selected.and_then(|index| reported_input(&read, index)))),
    });
    let mut page = RequestDetailsPage {
        run_epoch: read.epoch,
        request_id: read.request_id,
        revision: read.revision,
        availability: if read.documents.is_empty() {
            "unavailable"
        } else {
            "captured"
        },
        request_captured: read
            .documents
            .iter()
            .any(|d| matches!(d.document.source, DetailSource::Request { .. })),
        response_captured: read
            .documents
            .iter()
            .any(|d| matches!(d.document.source, DetailSource::Response { .. })),
        truncated: read.truncated,
        omitted: read.omitted,
        conflict: read.conflict,
        total_entries: entries.len(),
        capture_issues: if read.sources_partial && read.capture_issues.is_empty() {
            vec!["context_sources_partial".into()]
        } else {
            read.capture_issues
        },
        entries: Vec::new(),
        next_cursor: None,
        context: metadata,
    };
    let mut next = offset;
    for (document, entry) in entries.iter().skip(offset).take(16) {
        page.entries.push(ContextEntry {
            source: document.document.source.clone(),
            capture_seq: document.capture_seq,
            entry: (*entry).clone(),
        });
        page.next_cursor = (next + 1 < entries.len()).then(|| format!("{prefix}.{}", next + 1));
        if serde_json::to_vec(&page)
            .expect("closed details page")
            .len()
            > PAGE_BYTES
        {
            page.entries.pop();
            break;
        }
        next += 1;
    }
    page.next_cursor = (next < entries.len()).then(|| format!("{prefix}.{next}"));
    // First pages may carry a large summary. A metadata-only first page moves to
    // a continuation without that envelope; source entries retain their budget.
    if serde_json::to_vec(&page)
        .expect("closed details page")
        .len()
        > PAGE_BYTES
    {
        return Err(DetailError::InvalidCursor);
    }
    if next == offset && next < entries.len() && read.cursor.is_some() {
        return Err(DetailError::InvalidCursor);
    }
    Ok(page)
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
