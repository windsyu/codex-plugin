use super::*;
use crate::workbench::{decode::details, redaction::RedactionPolicy};
use serde_json::json;

struct Fixture {
    epoch: Uuid,
    request: Uuid,
    documents: Vec<(u64, DetailDocument)>,
    responses: Vec<Value>,
}
impl Fixture {
    fn new() -> Self {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        Self {
            epoch: Uuid::new_v4(),
            request: Uuid::new_v4(),
            responses: Vec::new(),
            documents: vec![(
                7,
                details::request(
                    &json!({"instructions":"Be helpful.","input":(0..40).map(|n| json!({"role":"user","content":format!("message {n}")})).collect::<Vec<_>>(),"tools":[{"type":"function","name":"read_file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}]}),
                    None,
                    &policy,
                ),
            )],
        }
    }
    fn read<'a>(
        &'a self,
        selection: &'a DetailSelection,
        cursor: Option<&'a str>,
    ) -> DetailRead<'a> {
        DetailRead {
            epoch: self.epoch,
            request_id: self.request,
            revision: 19,
            before: None,
            documents: self
                .documents
                .iter()
                .map(|(capture_seq, document)| DocumentRef {
                    document,
                    capture_seq: *capture_seq,
                })
                .collect(),
            responses: &self.responses,
            cursor,
            selection,
            truncated: false,
            omitted: false,
            conflict: false,
            capture_issues: Vec::new(),
            sources_partial: false,
        }
    }
    fn response(&mut self, input: u64) {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        self.documents.push((
            11,
            details::response(
                &json!({"id":"response","output":[],"usage":{"input_tokens":input}}),
                &policy,
            ),
        ));
        self.responses = vec![
            json!({"requestId":self.request,"responseId":"response","status":"completed","usage":{"inputTokens":input,"invalid":false},"usageConflict":false}),
        ];
    }
}

#[test]
fn selections_read_the_same_safe_source_and_cursors_bind_filters_and_history_window() {
    let fixture = Fixture::new();
    let all = DetailSelection::default();
    let first = read_page(fixture.read(&all, None)).unwrap();
    let source = first
        .context
        .as_ref()
        .unwrap()
        .selected_context
        .clone()
        .unwrap();
    let cursor = first.next_cursor.as_ref().unwrap();
    assert_eq!(first.entries.len(), 16);
    let next = read_page(fixture.read(&all, Some(cursor))).unwrap();
    assert!(next.context.is_none());
    let tools = DetailSelection {
        document: Some(source.clone()),
        category: Some("tools".into()),
        item: None,
    };
    assert_eq!(
        read_page(fixture.read(&tools, Some(cursor))).err(),
        Some(DetailError::InvalidCursor)
    );
    let selected = read_page(fixture.read(&tools, None)).unwrap();
    assert_eq!(selected.total_entries, 1);
    assert_eq!(selected.entries[0].entry.json_pointer, "/tools/0");
    let item = selected
        .context
        .unwrap()
        .context_summary
        .unwrap()
        .items
        .iter()
        .find(|i| i.item.kind.as_str() == "tools")
        .unwrap()
        .item
        .id
        .clone();
    let one = DetailSelection {
        document: Some(source),
        category: None,
        item: Some(item),
    };
    assert_eq!(
        read_page(fixture.read(&one, None)).unwrap().entries[0]
            .entry
            .json_pointer,
        "/tools/0"
    );
    let mut earlier = fixture.read(&all, Some(cursor));
    earlier.before = Some(1);
    assert_eq!(read_page(earlier).err(), Some(DetailError::InvalidCursor));
    let mut changed = fixture.read(&one, None);
    changed.revision += 1;
    assert_eq!(read_page(changed).err(), Some(DetailError::StaleCursor));
}

#[test]
fn scalar_input_items_link_to_the_existing_input_entry() {
    let mut fixture = Fixture::new();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    fixture.documents = vec![(
        1,
        details::request(&json!({"input":"a scalar user message"}), None, &policy),
    )];
    let all = DetailSelection::default();
    let page = read_page(fixture.read(&all, None)).unwrap();
    let metadata = page.context.unwrap();
    let item = &metadata.context_summary.unwrap().items[0].item;
    assert_eq!(item.references, ["/input"]);
    assert!(!item.references_partial);
    let selected = DetailSelection {
        document: metadata.selected_context,
        category: None,
        item: Some(item.id.clone()),
    };
    let page = read_page(fixture.read(&selected, None)).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].entry.json_pointer, "/input");
    assert!(
        page.entries[0]
            .entry
            .preview
            .contains("a scalar user message")
    );
}

#[test]
fn namespace_description_drilldown_keeps_its_parent_entry() {
    let mut fixture = Fixture::new();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    fixture.documents = vec![(
        1,
        details::request(
            &json!({"tools":[{"type":"namespace","name":"functions","description":"NAMESPACE_DESCRIPTION","tools":[{"type":"function","name":"read_file","parameters":{}}]}]}),
            None,
            &policy,
        ),
    )];
    let all = DetailSelection::default();
    let metadata = read_page(fixture.read(&all, None))
        .unwrap()
        .context
        .unwrap();
    let summary = metadata.context_summary.unwrap();
    let item = summary
        .items
        .iter()
        .find(|item| item.item.name == "functions")
        .unwrap();
    let selection = DetailSelection {
        document: metadata.selected_context,
        item: Some(item.item.id.clone()),
        ..Default::default()
    };
    let page = read_page(fixture.read(&selection, None)).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].entry.json_pointer, "/tools/0");
    assert!(
        page.entries[0]
            .entry
            .preview
            .contains("NAMESPACE_DESCRIPTION")
    );
    assert!(!page.entries[0].entry.preview.contains("read_file"));
}

#[test]
fn clipped_documents_keep_only_retained_references_without_changing_estimates() {
    let policy = RedactionPolicy::new(vec![]).unwrap();
    for many_messages in [false, true] {
        let input = if many_messages {
            let mut messages = vec![json!({"role":"user","content":"x"}); 1024];
            messages.push(json!({"role":"user","content":"x".repeat(300)}));
            messages
        } else {
            vec![json!({"role":"user","content":"x".repeat(30_000)}); 20]
        };
        let body = json!({"input":input});
        let original = crate::workbench::context::analyze(&body, &policy);
        let document = details::request(&body, None, &policy);
        assert!(document.truncated);
        // The many-message case also exceeds the input traversal cap; the
        // per-entry overhead exhausts the byte budget before 1024 entries.
        assert!(document.entries.len() < 1024);
        let analysis = document.context_analysis.as_ref().unwrap();
        assert!(analysis.summary == original.summary);
        assert!(analysis.summary.coverage.complete);
        assert_eq!(
            analysis.summary.content_bytes,
            if many_messages { 1324 } else { 600_000 }
        );
        assert_eq!(
            analysis.summary.estimated_tokens,
            if many_messages { 1099 } else { 150_000 }
        );
        assert_eq!(analysis.items.len(), original.items.len());
        for (item, raw) in analysis.items.iter().zip(&original.items) {
            assert_eq!(item.id, raw.id);
            assert_eq!(item.occurrences, raw.occurrences);
            assert_eq!(item.content_bytes, raw.content_bytes);
            assert_eq!(item.estimated_tokens, raw.estimated_tokens);
            assert!(
                item.references.iter().all(|reference| document
                    .entries
                    .iter()
                    .any(|entry| entry.json_pointer == *reference)),
                "dangling reference in {}",
                item.name
            );
        }
        let missing_name = if many_messages {
            "消息 1025"
        } else {
            "消息 20"
        };
        let missing = analysis
            .items
            .iter()
            .find(|item| item.name == missing_name)
            .unwrap();
        assert!(missing.references.is_empty());
        assert!(missing.references_partial);
        let retained = analysis
            .items
            .iter()
            .find(|item| {
                item.references
                    .iter()
                    .any(|reference| reference == "/input/0")
            })
            .unwrap();
        let retained_id = retained.id.clone();
        let missing_id = missing.id.clone();

        let saved = serde_json::to_value(&document).unwrap();
        let restored: DetailDocument = serde_json::from_value(saved).unwrap();
        assert!(restored == document);
        let mut fixture = Fixture::new();
        fixture.documents = vec![(1, restored)];
        fixture.response(101);
        let all = DetailSelection::default();
        let metadata = read_page(fixture.read(&all, None))
            .unwrap()
            .context
            .unwrap();
        let summary = metadata.context_summary.unwrap();
        assert!(summary.calibrated);
        assert_eq!(summary.tokens, 101);
        assert_eq!(summary.estimated_tokens, original.summary.estimated_tokens);
        assert_eq!(
            summary.items.iter().map(|item| item.tokens).sum::<u64>(),
            101
        );
        let mut selection = DetailSelection {
            document: metadata.selected_context,
            item: Some(retained_id),
            ..Default::default()
        };
        let retained_page = read_page(fixture.read(&selection, None)).unwrap();
        assert!(
            retained_page
                .entries
                .iter()
                .any(|entry| entry.entry.json_pointer == "/input/0")
        );
        selection.item = Some(missing_id);
        let missing_page = read_page(fixture.read(&selection, None)).unwrap();
        assert_eq!(missing_page.total_entries, 0);
        assert!(missing_page.entries.is_empty());
    }
}

#[test]
fn creates_in_one_observation_have_distinct_document_selectors_and_never_borrow_usage() {
    let mut fixture = Fixture::new();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    fixture.documents = vec![
        (
            7,
            details::request(&json!({"input":"first"}), Some(1), &policy),
        ),
        (
            7,
            details::request(
                &json!({"input":"a longer second message"}),
                Some(2),
                &policy,
            ),
        ),
    ];
    fixture.response(500);
    let all = DetailSelection::default();
    let page = read_page(fixture.read(&all, None)).unwrap();
    let metadata = page.context.unwrap();
    assert_eq!(metadata.context_sources.len(), 2);
    assert_ne!(
        metadata.context_sources[0].id,
        metadata.context_sources[1].id
    );
    assert_eq!(
        metadata.context_sources[0].capture_seq,
        metadata.context_sources[1].capture_seq
    );
    assert!(!metadata.context_summary.unwrap().calibrated);
    let first = DetailSelection {
        document: Some(metadata.context_sources[0].id.clone()),
        ..Default::default()
    };
    let first = read_page(fixture.read(&first, None)).unwrap();
    assert_eq!(first.total_entries, 2);
    assert!(
        first
            .entries
            .iter()
            .any(|e| e.entry.preview.contains("first"))
    );
    assert!(
        !first
            .entries
            .iter()
            .any(|e| e.entry.preview.contains("longer"))
    );
}

#[test]
fn calibration_uses_reported_input_once_and_preserves_estimates_on_unknown_or_conflicting_evidence()
{
    let mut fixture = Fixture::new();
    fixture.response(101);
    let all = DetailSelection::default();
    let mut read = fixture.read(&all, None);
    read.truncated = true; // Reading previews may be cut while classification is complete.
    let summary = read_page(read)
        .unwrap()
        .context
        .unwrap()
        .context_summary
        .unwrap();
    assert!(summary.calibrated);
    assert_eq!(summary.tokens, 101);
    assert_eq!(summary.parts.iter().map(|p| p.tokens).sum::<u64>(), 101);
    assert_eq!(summary.items.iter().map(|i| i.tokens).sum::<u64>(), 101);
    fixture.responses[0]["usage"]["inputTokens"] = json!(0);
    assert_eq!(
        read_page(fixture.read(&all, None))
            .unwrap()
            .context
            .unwrap()
            .context_summary
            .unwrap()
            .tokens,
        0
    );
    fixture.responses[0]["usageConflict"] = json!(true);
    let summary = read_page(fixture.read(&all, None))
        .unwrap()
        .context
        .unwrap()
        .context_summary
        .unwrap();
    assert!(!summary.calibrated);
    assert_eq!(summary.tokens, summary.estimated_tokens);
    fixture.responses[0]["usageConflict"] = json!(false);
    let mut read = fixture.read(&all, None);
    read.sources_partial = true;
    assert!(
        !read_page(read)
            .unwrap()
            .context
            .unwrap()
            .context_summary
            .unwrap()
            .calibrated
    );
    let mut read = fixture.read(&all, None);
    read.capture_issues.push("observation_gap".into());
    assert!(
        !read_page(read)
            .unwrap()
            .context
            .unwrap()
            .context_summary
            .unwrap()
            .calibrated
    );
}

#[test]
fn large_pages_include_metadata_in_the_budget_and_old_documents_remain_readable() {
    let mut fixture = Fixture::new();
    let policy = RedactionPolicy::new(vec![]).unwrap();
    fixture.documents = vec![(
        1,
        details::request(
            &json!({"input":vec![json!({"role":"user","content":"中\\\"\n".repeat(12_000)});50]}),
            None,
            &policy,
        ),
    )];
    let all = DetailSelection::default();
    let mut cursor = None;
    let mut count = 0;
    for _ in 0..100 {
        let page = read_page(fixture.read(&all, cursor.as_deref())).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_BYTES);
        count += page.entries.len();
        if page.next_cursor.is_none() {
            break;
        }
        if cursor.is_some() {
            assert_ne!(cursor, page.next_cursor);
        }
        cursor = page.next_cursor;
    }
    assert_eq!(count, fixture.documents[0].1.entries.len());
    fixture.documents[0].1.context_analysis = None;
    let page = read_page(fixture.read(&all, None)).unwrap();
    assert!(!page.context.as_ref().unwrap().context_sources[0].available);
    assert!(page.context.as_ref().unwrap().context_summary.is_none());
    assert!(!page.entries.is_empty());
}
