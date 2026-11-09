//! Memory-only on-demand context. Large documents never enter the live ring.
use super::*;
use crate::workbench::decode::details::DetailDocument;
use crate::workbench::decode::details::page::{self, DetailRead, DetailSelection, DocumentRef};
pub use crate::workbench::decode::details::page::{DetailError, RequestDetailsPage};

const REQUESTS: usize = 64;
const TOTAL_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_BYTES: usize = 2 * 1024 * 1024;
#[cfg(test)]
const PAGE_BYTES: usize = page::PAGE_BYTES;

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
    sources_partial: bool,
}
#[derive(Default)]
pub(super) struct DetailStore {
    bundles: VecDeque<Bundle>,
    bytes: usize,
    revision: u64,
    evicted: VecDeque<Uuid>,
    diagnostics_partial: bool,
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
        self.diagnostics_partial
            || !self.evicted.is_empty()
            || self.bundles.iter().any(|b| b.sources_partial)
    }
    pub(super) fn partial_requests(&self) -> Option<Vec<Uuid>> {
        (!self.diagnostics_partial).then(|| {
            let mut requests: Vec<_> = self.evicted.iter().copied().collect();
            requests.extend(
                self.bundles
                    .iter()
                    .filter(|b| b.sources_partial)
                    .map(|b| b.request_id),
            );
            requests
        })
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
                sources_partial: self.evicted.contains(&request_id),
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
                bundle.sources_partial = true;
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
                self.diagnostics_partial = true;
                self.invalidate(None);
            }
        }
    }

    pub(super) fn invalidate(&mut self, request: Option<Uuid>) {
        self.revision += 1;
        for bundle in &mut self.bundles {
            if request.is_none_or(|id| bundle.request_id == id) {
                bundle.revision = self.revision;
            }
        }
    }

    pub(super) fn lose_diagnostic(&mut self, request: Uuid) {
        self.revision += 1;
        let unscoped = request.is_nil()
            || !self
                .bundles
                .iter()
                .any(|bundle| bundle.request_id == request);
        self.diagnostics_partial |= unscoped;
        for bundle in &mut self.bundles {
            if unscoped || bundle.request_id == request {
                bundle.sources_partial = true;
                bundle.revision = self.revision;
            }
        }
    }

    #[cfg(test)]
    fn page(
        &self,
        epoch: Uuid,
        request_id: Uuid,
        cursor: Option<&str>,
    ) -> Result<RequestDetailsPage, DetailError> {
        self.page_with(
            epoch,
            request_id,
            cursor,
            &DetailSelection::default(),
            &[],
            Vec::new(),
        )
    }

    fn page_with(
        &self,
        epoch: Uuid,
        request_id: Uuid,
        cursor: Option<&str>,
        selection: &DetailSelection,
        responses: &[serde_json::Value],
        issues: Vec<String>,
    ) -> Result<RequestDetailsPage, DetailError> {
        let Some(bundle) = self.bundles.iter().find(|b| b.request_id == request_id) else {
            return Err(if self.evicted.contains(&request_id) {
                DetailError::Evicted
            } else {
                DetailError::NotFound
            });
        };
        page::read_page(DetailRead {
            epoch,
            request_id,
            revision: bundle.revision,
            before: None,
            documents: bundle
                .documents
                .iter()
                .map(|d| DocumentRef {
                    document: &d.document,
                    capture_seq: d.capture_seq,
                })
                .collect(),
            responses,
            cursor,
            selection,
            truncated: bundle.truncated,
            omitted: bundle.omitted,
            conflict: bundle.conflict,
            capture_issues: issues,
            sources_partial: self.diagnostics_partial || bundle.sources_partial,
        })
    }
}

impl LiveHub {
    pub fn request_details(
        &self,
        request_id: Uuid,
        cursor: Option<&str>,
    ) -> Result<RequestDetailsPage, DetailError> {
        self.request_details_selected(request_id, cursor, &DetailSelection::default())
    }

    pub(crate) fn request_details_selected(
        &self,
        request_id: Uuid,
        cursor: Option<&str>,
        selection: &DetailSelection,
    ) -> Result<RequestDetailsPage, DetailError> {
        let state = self.state.lock().unwrap();
        let issues: Vec<String> = state
            .diagnostics
            .iter()
            .filter(|d| d.request_id == request_id || d.request_id.is_nil())
            .take(16)
            .map(|d| {
                serde_json::to_value(d.code)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let responses: Vec<_> = state
            .responses
            .iter()
            .filter(|r| r.request_id == request_id)
            .map(|r| serde_json::to_value(r).expect("closed response"))
            .collect();
        match state.details.page_with(
            self.epoch,
            request_id,
            cursor,
            selection,
            &responses,
            issues.clone(),
        ) {
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
                if cursor.is_some() || selection.document.is_some() {
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
                    capture_issues: issues,
                    entries: Vec::new(),
                    next_cursor: None,
                    context: Some(page::ContextMetadata {
                        context_sources: Vec::new(),
                        selected_context: None,
                        context_summary: None,
                    }),
                })
            }
            result => result,
        }
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

    #[test]
    fn diagnostic_eviction_does_not_restore_calibration_or_keep_old_cursors_valid() {
        for global in [false, true] {
            let hub = LiveHub::new(Default::default());
            let request = Uuid::new_v4();
            let policy = RedactionPolicy::new(vec![]).unwrap();
            let apply = |request_id, change| {
                hub.apply(Decoded {
                    request_id,
                    capture_seq: 1,
                    received_at: Instant::now(),
                    change,
                })
            };
            apply(
                request,
                Change::Document {
                    document: details::request(
                        &json!({"input":vec![json!({"role":"user","content":"hello"});40]}),
                        None,
                        &policy,
                    ),
                },
            );
            apply(
                request,
                Change::Document {
                    document: details::response(&json!({"id":"completed","output":[]}), &policy),
                },
            );
            apply(
                request,
                Change::Response {
                    response_id: Some("completed".into()),
                    status: ResponseStatus::Completed,
                    model: None,
                    usage: details::usage(&json!({"input_tokens":101})),
                },
            );
            assert!(
                hub.request_details(request, None)
                    .unwrap()
                    .context
                    .unwrap()
                    .context_summary
                    .unwrap()
                    .calibrated
            );
            apply(
                if global { Uuid::nil() } else { request },
                Change::Diagnostic {
                    code: DiagnosticCode::ObservationGap,
                },
            );
            let incomplete = hub.request_details(request, None).unwrap();
            assert!(
                !incomplete
                    .context
                    .unwrap()
                    .context_summary
                    .unwrap()
                    .calibrated
            );
            let cursor = incomplete.next_cursor.unwrap();
            for _ in 0..128 {
                apply(
                    Uuid::new_v4(),
                    Change::Diagnostic {
                        code: DiagnosticCode::InvalidJson,
                    },
                );
            }
            let page = hub.request_details(request, None).unwrap();
            assert!(!page.context.unwrap().context_summary.unwrap().calibrated);
            assert_eq!(
                hub.request_details(request, Some(&cursor)).err(),
                Some(DetailError::StaleCursor)
            );
            assert!(hub.recording_checkpoint().details_partial);
        }
    }

    #[test]
    fn diagnostics_lost_before_any_document_keep_later_sources_uncertain() {
        for global in [false, true] {
            let hub = LiveHub::new(Default::default());
            let request = Uuid::new_v4();
            let apply = |request_id, change| {
                hub.apply(Decoded {
                    request_id,
                    capture_seq: 1,
                    received_at: Instant::now(),
                    change,
                })
            };
            apply(
                if global { Uuid::nil() } else { request },
                Change::Diagnostic {
                    code: DiagnosticCode::ObservationGap,
                },
            );
            for _ in 0..128 {
                apply(
                    Uuid::new_v4(),
                    Change::Diagnostic {
                        code: DiagnosticCode::InvalidJson,
                    },
                );
            }
            let policy = RedactionPolicy::new(vec![]).unwrap();
            apply(
                request,
                Change::Document {
                    document: details::request(&json!({"input":"hello"}), None, &policy),
                },
            );
            apply(
                request,
                Change::Document {
                    document: details::response(&json!({"id":"completed","output":[]}), &policy),
                },
            );
            apply(
                request,
                Change::Response {
                    response_id: Some("completed".into()),
                    status: ResponseStatus::Completed,
                    model: None,
                    usage: details::usage(&json!({"input_tokens":101})),
                },
            );
            let page = hub.request_details(request, None).unwrap();
            assert!(!page.context.unwrap().context_summary.unwrap().calibrated);
            assert_eq!(page.capture_issues, ["context_sources_partial"]);
            let checkpoint = hub.recording_checkpoint();
            assert!(checkpoint.details_partial);
            assert!(checkpoint.details_partial_requests().is_none());
        }
    }
}
