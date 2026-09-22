//! Run-scoped reported usage, independent of the evictable reading preview.
use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use crate::workbench::decode::{DiagnosticCode, ResponseStatus, details::ResponseUsage};

const MAX_RESPONSES: usize = 16_384;
const MAX_SAFE_INTEGER: u128 = 9_007_199_254_740_991;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetric {
    // None means no report or an aggregate beyond the JSON safe-integer range.
    pub tokens: Option<u64>,
    pub responses: u32,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub response_count: u32,
    pub missing_responses: u32,
    pub excluded_responses: u32,
    pub input_tokens: UsageMetric,
    pub output_tokens: UsageMetric,
    pub total_tokens: UsageMetric,
    pub cached_input_tokens: UsageMetric,
    pub cache_write_tokens: UsageMetric,
    pub reasoning_tokens: UsageMetric,
    pub capture_incomplete: bool,
    pub unidentified_response: bool,
    pub capacity_exceeded: bool,
}

#[derive(Default)]
struct Entry {
    usage: Option<ResponseUsage>,
    conflict: bool,
}
impl Entry {
    fn excluded(&self) -> bool {
        self.conflict || self.usage.as_ref().is_some_and(|u| u.invalid)
    }
    fn values(&self) -> [Option<u64>; 6] {
        if self.excluded() {
            return [None; 6];
        }
        let Some(u) = &self.usage else {
            return [None; 6];
        };
        [
            u.input_tokens,
            u.output_tokens,
            u.total_tokens.or_else(|| {
                u.input_tokens
                    .zip(u.output_tokens)
                    .and_then(|(input, output)| input.checked_add(output))
            }),
            u.cached_input_tokens,
            u.cache_write_tokens,
            u.reasoning_tokens,
        ]
    }
}

#[derive(Default)]
pub(super) struct RunUsage {
    entries: HashMap<(Uuid, String), Entry>,
    // u128 permits exact removal of a later-conflicting report even when the
    // cumulative value has exceeded the browser's safe-integer range.
    sums: [u128; 6],
    reports: [u32; 6],
    missing: u32,
    excluded: u32,
    capture_incomplete: bool,
    unidentified: bool,
    capped: bool,
}
impl RunUsage {
    pub fn observe(
        &mut self,
        request: Uuid,
        response: Option<&str>,
        status: ResponseStatus,
        usage: Option<&ResponseUsage>,
    ) -> (Option<ResponseUsage>, bool) {
        let Some(response) = response else {
            self.unidentified |= status != ResponseStatus::Receiving;
            return (usage.cloned(), false);
        };
        let key = (request, response.to_owned());
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_RESPONSES {
            // Retain identity evidence for already-counted responses. Evicting
            // it would let a late duplicate silently inflate the totals.
            self.capped = true;
            return (usage.cloned(), false);
        }
        let mut entry = if let Some(previous) = self.entries.remove(&key) {
            self.account(&previous, false);
            previous
        } else {
            Entry::default()
        };
        if let Some(report) = usage {
            if let Some(previous) = &entry.usage {
                entry.conflict |= previous != report;
            } else {
                entry.usage = Some(report.clone());
            }
        }
        self.account(&entry, true);
        let evidence = (entry.usage.clone(), entry.conflict);
        self.entries.insert(key, entry);
        evidence
    }
    fn account(&mut self, entry: &Entry, add: bool) {
        let values = entry.values();
        let adjust = |count: &mut u32| {
            if add { *count += 1 } else { *count -= 1 }
        };
        if entry.excluded() {
            adjust(&mut self.excluded);
        } else if values.iter().all(Option::is_none) {
            adjust(&mut self.missing);
        }
        for (index, value) in values.into_iter().enumerate() {
            if let Some(value) = value {
                if add {
                    self.sums[index] += u128::from(value);
                } else {
                    self.sums[index] -= u128::from(value);
                }
                adjust(&mut self.reports[index]);
            }
        }
    }
    pub fn diagnostic(&mut self, code: DiagnosticCode) {
        // Omitted media/reasoning/context is not proof of missing usage.
        self.capture_incomplete |= code != DiagnosticCode::OmittedByPolicy;
    }
    pub fn summary(&self) -> UsageSummary {
        let metric = |index: usize| UsageMetric {
            tokens: (self.reports[index] > 0 && self.sums[index] <= MAX_SAFE_INTEGER)
                .then_some(self.sums[index] as u64),
            responses: self.reports[index],
        };
        UsageSummary {
            response_count: self.entries.len() as u32,
            missing_responses: self.missing,
            excluded_responses: self.excluded,
            input_tokens: metric(0),
            output_tokens: metric(1),
            total_tokens: metric(2),
            cached_input_tokens: metric(3),
            cache_write_tokens: metric(4),
            reasoning_tokens: metric(5),
            capture_incomplete: self.capture_incomplete,
            unidentified_response: self.unidentified,
            capacity_exceeded: self.capped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::decode::details::usage;
    use serde_json::json;

    #[test]
    fn unknown_zero_partial_invalid_conflicting_and_unidentified_usage_stay_distinct() {
        let mut run = RunUsage::default();
        let id = Uuid::new_v4();
        let emit = |run: &mut RunUsage, name, value| {
            run.observe(id, name, ResponseStatus::Completed, usage(&value).as_ref());
        };
        emit(&mut run, Some("missing"), json!(null));
        emit(
            &mut run,
            Some("zero"),
            json!({"input_tokens":0,"output_tokens":0}),
        );
        emit(&mut run, Some("partial"), json!({"input_tokens":10}));
        emit(&mut run, Some("invalid"), json!({"input_tokens":-1}));
        emit(&mut run, None, json!({"total_tokens":999}));
        let before = run.summary();
        assert_eq!(before.response_count, 4);
        assert_eq!(before.missing_responses, 1);
        assert_eq!(before.excluded_responses, 1);
        assert_eq!(before.input_tokens.tokens, Some(10));
        assert_eq!(before.input_tokens.responses, 2);
        assert_eq!(before.total_tokens.tokens, Some(0));
        assert_eq!(before.total_tokens.responses, 1);
        assert_eq!(before.reasoning_tokens.tokens, None);
        assert!(before.unidentified_response);
        emit(&mut run, Some("partial"), json!({"input_tokens":11}));
        emit(&mut run, Some("zero"), json!(null));
        assert_eq!(run.summary().input_tokens.tokens, Some(0));
        assert_eq!(run.summary().excluded_responses, 2);
    }

    #[test]
    fn bounds_keep_deduplication_and_overflow_never_becomes_a_rounded_total() {
        let mut run = RunUsage::default();
        let id = Uuid::new_v4();
        let large = usage(&json!({"total_tokens":MAX_SAFE_INTEGER as u64}));
        for index in 0..MAX_RESPONSES + 1 {
            run.observe(
                id,
                Some(&index.to_string()),
                ResponseStatus::Completed,
                large.as_ref(),
            );
        }
        let summary = run.summary();
        assert_eq!(summary.response_count, MAX_RESPONSES as u32);
        assert!(summary.capacity_exceeded);
        assert_eq!(summary.total_tokens.tokens, None);
        assert_eq!(summary.total_tokens.responses, MAX_RESPONSES as u32);
        run.observe(id, Some("0"), ResponseStatus::Completed, large.as_ref());
        assert_eq!(run.summary().response_count, MAX_RESPONSES as u32);
        assert_eq!(run.summary().excluded_responses, 0);
        run.diagnostic(DiagnosticCode::OmittedByPolicy);
        assert!(!run.summary().capture_incomplete);
        run.diagnostic(DiagnosticCode::ObservationGap);
        assert!(run.summary().capture_incomplete);
    }
}
