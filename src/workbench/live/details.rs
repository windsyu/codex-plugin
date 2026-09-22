//! Memory-only on-demand context. Large documents never enter the live ring.
use super::*;
use crate::workbench::decode::details::{DetailDocument, DetailEntry, DetailSource};

const REQUESTS: usize = 64;
const TOTAL_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_BYTES: usize = 2 * 1024 * 1024;
const PAGE_BYTES: usize = 128 * 1024;

struct StoredDocument {
    document: DetailDocument,
    capture_seq: u64,
}
struct Bundle {
    request_id: Uuid,
    revision: u64,
    documents: Vec<StoredDocument>,
    bytes: usize,
    truncated: bool,
    omitted: bool,
    conflict: bool,
}
#[derive(Default)]
pub(super) struct DetailStore {
    bundles: VecDeque<Bundle>,
    bytes: usize,
    revision: u64,
    evicted: VecDeque<Uuid>,
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
    pub capture_issues: Vec<DiagnosticCode>,
    pub entries: Vec<ContextEntry>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum DetailError {
    NotFound,
    Evicted,
    InvalidCursor,
    StaleCursor,
}

impl DetailStore {
    pub(super) fn checkpoint(&self) -> Vec<super::super::recording::replay::Document> {
        self.bundles
            .iter()
            .flat_map(|bundle| {
                bundle
                    .documents
                    .iter()
                    .map(|d| super::super::recording::replay::Document {
                        request_id: bundle.request_id,
                        capture_seq: d.capture_seq,
                        document: serde_json::json!(d.document),
                    })
            })
            .collect()
    }
    pub(super) fn partial(&self) -> bool {
        !self.evicted.is_empty() || self.bundles.iter().any(|b| b.truncated)
    }
    pub fn insert(&mut self, request_id: Uuid, capture_seq: u64, document: DetailDocument) {
        let index = self
            .bundles
            .iter()
            .position(|bundle| bundle.request_id == request_id);
        let mut bundle = index
            .and_then(|index| self.bundles.remove(index))
            .unwrap_or_else(|| Bundle {
                request_id,
                revision: 0,
                documents: Vec::new(),
                bytes: 0,
                truncated: self.evicted.contains(&request_id),
                omitted: false,
                conflict: false,
            });
        self.bytes -= bundle.bytes;
        if !bundle
            .documents
            .iter()
            .any(|entry| entry.document == document)
        {
            self.revision += 1;
            bundle.revision = self.revision;
            bundle.conflict |= bundle
                .documents
                .iter()
                .any(|entry| entry.document.source == document.source);
            bundle.truncated |= document.truncated;
            bundle.omitted |= document.omitted;
            let bytes = document.bytes();
            if bundle.bytes + bytes <= REQUEST_BYTES && bundle.documents.len() < 128 {
                bundle.bytes += bytes;
                bundle.documents.push(StoredDocument {
                    document,
                    capture_seq,
                });
            } else {
                bundle.truncated = true;
            }
        }
        self.bytes += bundle.bytes;
        self.bundles.push_back(bundle);
        self.evicted.retain(|id| *id != request_id);
        while self.bundles.len() > REQUESTS || self.bytes > TOTAL_BYTES {
            let removed = self.bundles.pop_front().unwrap();
            self.bytes -= removed.bytes;
            self.evicted.push_back(removed.request_id);
            if self.evicted.len() > 256 {
                self.evicted.pop_front();
            }
        }
    }

    fn page(
        &self,
        epoch: Uuid,
        request_id: Uuid,
        cursor: Option<&str>,
    ) -> Result<RequestDetailsPage, DetailError> {
        let Some(bundle) = self
            .bundles
            .iter()
            .find(|bundle| bundle.request_id == request_id)
        else {
            return Err(if self.evicted.contains(&request_id) {
                DetailError::Evicted
            } else {
                DetailError::NotFound
            });
        };
        let offset = if let Some(cursor) = cursor {
            let fields: Vec<_> = cursor.split('.').collect();
            if fields.len() != 4
                || fields[0].parse::<Uuid>().ok() != Some(epoch)
                || fields[1].parse::<Uuid>().ok() != Some(request_id)
            {
                return Err(DetailError::InvalidCursor);
            }
            let revision = fields[2]
                .parse::<u64>()
                .map_err(|_| DetailError::InvalidCursor)?;
            let offset = fields[3]
                .parse::<usize>()
                .map_err(|_| DetailError::InvalidCursor)?;
            if revision != bundle.revision {
                return Err(DetailError::StaleCursor);
            }
            offset
        } else {
            0
        };
        let total_entries = bundle
            .documents
            .iter()
            .map(|entry| entry.document.entries.len())
            .sum();
        if offset > total_entries {
            return Err(DetailError::InvalidCursor);
        }
        let mut entries = Vec::new();
        let mut bytes = 1024; // Reserve the envelope as well as each entry's source.
        for (document, entry) in bundle
            .documents
            .iter()
            .flat_map(|document| {
                document
                    .document
                    .entries
                    .iter()
                    .map(move |entry| (document, entry))
            })
            .skip(offset)
        {
            let cost = entry.preview.len() * 2 + 768;
            if entries.len() >= 16 || bytes + cost > PAGE_BYTES {
                break;
            }
            bytes += cost;
            entries.push(ContextEntry {
                source: document.document.source.clone(),
                capture_seq: document.capture_seq,
                entry: entry.clone(),
            });
        }
        let next = offset + entries.len();
        Ok(RequestDetailsPage {
            run_epoch: epoch,
            request_id,
            revision: bundle.revision,
            availability: "captured",
            request_captured: bundle
                .documents
                .iter()
                .any(|entry| matches!(entry.document.source, DetailSource::Request { .. })),
            response_captured: bundle
                .documents
                .iter()
                .any(|entry| matches!(entry.document.source, DetailSource::Response { .. })),
            truncated: bundle.truncated,
            omitted: bundle.omitted,
            conflict: bundle.conflict,
            total_entries,
            capture_issues: Vec::new(),
            entries,
            next_cursor: (next < total_entries)
                .then(|| format!("{epoch}.{request_id}.{}.{next}", bundle.revision)),
        })
    }
}

impl LiveHub {
    pub fn request_details(
        &self,
        request_id: Uuid,
        cursor: Option<&str>,
    ) -> Result<RequestDetailsPage, DetailError> {
        let state = self.state.lock().unwrap();
        let mut result = match state.details.page(self.epoch, request_id, cursor) {
            Err(DetailError::NotFound)
                if state
                    .requests
                    .iter()
                    .any(|entry| entry.request_id == request_id)
                    || state
                        .responses
                        .iter()
                        .any(|entry| entry.request_id == request_id)
                    || state
                        .diagnostics
                        .iter()
                        .any(|entry| entry.request_id == request_id) =>
            {
                if cursor.is_some() {
                    return Err(DetailError::StaleCursor);
                }
                let unavailable = state
                    .diagnostics
                    .iter()
                    .any(|entry| entry.request_id == request_id)
                    || state
                        .requests
                        .iter()
                        .any(|entry| entry.request_id == request_id)
                    || state.responses.iter().any(|entry| {
                        entry.request_id == request_id && entry.status != ResponseStatus::Receiving
                    });
                Ok(RequestDetailsPage {
                    run_epoch: self.epoch,
                    request_id,
                    revision: 0,
                    availability: if unavailable {
                        "unavailable"
                    } else {
                        "pending"
                    },
                    request_captured: false,
                    response_captured: false,
                    truncated: unavailable,
                    omitted: false,
                    conflict: false,
                    total_entries: 0,
                    capture_issues: Vec::new(),
                    entries: Vec::new(),
                    next_cursor: None,
                })
            }
            result => result,
        };
        if let Ok(page) = &mut result {
            page.capture_issues = state
                .diagnostics
                .iter()
                .filter(|entry| entry.request_id == request_id)
                .take(16)
                .map(|entry| entry.code)
                .collect();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::decode::details;
    use crate::workbench::redaction::RedactionPolicy;
    use serde_json::json;
    #[test]
    fn pages_are_scoped_bounded_and_invalidated_when_new_response_context_arrives() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        let mut store = DetailStore::default();
        let (epoch, id) = (Uuid::new_v4(), Uuid::new_v4());
        let document = details::request(
            &json!({"input":(0..40).map(|index| json!({"role":"user","content":format!("message-{index}")})).collect::<Vec<_>>()}),
            None,
            &policy,
        );
        store.insert(id, 1, document.clone());
        let first = store.page(epoch, id, None).unwrap();
        assert_eq!(first.entries.len(), 16);
        let cursor = first.next_cursor.unwrap();
        let second = store.page(epoch, id, Some(&cursor)).unwrap();
        assert_eq!(second.entries[0].entry.position, Some(15));
        assert!(serde_json::to_vec(&second).unwrap().len() <= PAGE_BYTES + 1024);
        store.insert(id, 2, document);
        assert!(store.page(epoch, id, Some(&cursor)).is_ok());
        assert_eq!(
            store.page(Uuid::new_v4(), id, Some(&cursor)).err(),
            Some(DetailError::InvalidCursor)
        );
        store.insert(
            id,
            3,
            details::response(&json!({"id":"response","output":[]}), &policy),
        );
        assert_eq!(
            store.page(epoch, id, Some(&cursor)).err(),
            Some(DetailError::StaleCursor)
        );
    }
    #[test]
    fn request_and_global_budgets_leave_eviction_and_truncation_visible() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        let mut store = DetailStore::default();
        let (epoch, first) = (Uuid::new_v4(), Uuid::new_v4());
        store.insert(
            first,
            1,
            details::request(&json!({"input":[]}), None, &policy),
        );
        for _ in 0..65 {
            store.insert(
                Uuid::new_v4(),
                1,
                details::request(&json!({"input":[]}), None, &policy),
            );
        }
        assert_eq!(
            store.page(epoch, first, None).err(),
            Some(DetailError::Evicted)
        );
        for index in 0..130 {
            store.insert(
                first,
                1,
                details::request(
                    &json!({"input":[{"content":"x".repeat(30000)}]}),
                    Some(index),
                    &policy,
                ),
            );
        }
        assert!(store.page(epoch, first, None).unwrap().truncated);
        assert!(store.bytes <= TOTAL_BYTES && store.bundles.len() <= REQUESTS);
    }
}
