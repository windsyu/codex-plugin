//! Integer apportionment of an estimate, never independent reported usage.
use super::*;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

// Integer largest remainder. Input order is the stable tie break; zero weights
// receive nothing. All products fit u128, including JSON's largest safe count.
fn allocate(weights: &[u64], total: u64) -> Vec<u64> {
    let sum: u128 = weights.iter().map(|n| u128::from(*n)).sum();
    if sum == 0 {
        return vec![0; weights.len()];
    }
    let mut out = vec![0; weights.len()];
    let mut remainder = Vec::new();
    for (index, weight) in weights.iter().enumerate() {
        let product = u128::from(*weight) * u128::from(total);
        out[index] = (product / sum) as u64;
        if *weight > 0 {
            remainder.push((product % sum, index));
        }
    }
    remainder.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let left = total - out.iter().sum::<u64>();
    for (_, index) in remainder.into_iter().take(left as usize) {
        out[index] += 1;
    }
    out
}

pub(crate) fn present(
    analysis: &ContextAnalysis,
    reported_input: Option<u64>,
) -> ContextPresentation {
    let summary = &analysis.summary;
    let reported = reported_input.filter(|n| *n <= MAX_SAFE_INTEGER);
    // Validate persisted weights as well: older/unknown schemas and malformed
    // saved analyses remain estimates instead of fabricating calibrated totals.
    let consistent = summary.schema_version == 1
        && summary.method_version == 1
        && summary
            .parts
            .iter()
            .map(|p| u128::from(p.estimated_tokens))
            .sum::<u128>()
            == u128::from(summary.estimated_tokens)
        && summary.parts.iter().all(|part| {
            analysis
                .items
                .iter()
                .filter(|i| i.kind == part.kind)
                .map(|i| u128::from(i.estimated_tokens))
                .sum::<u128>()
                == u128::from(part.estimated_tokens)
        })
        && analysis
            .items
            .iter()
            .all(|item| summary.parts.iter().filter(|p| p.kind == item.kind).count() == 1)
        && summary
            .parts
            .iter()
            .enumerate()
            .all(|(index, part)| !summary.parts[..index].iter().any(|p| p.kind == part.kind));
    let calibrated = summary.coverage.complete
        && summary.coverage.issues.is_empty()
        && summary.estimated_tokens > 0
        && reported.is_some()
        && consistent;
    let tokens = if calibrated {
        reported.unwrap()
    } else {
        summary.estimated_tokens
    };
    let part_weights: Vec<_> = summary.parts.iter().map(|p| p.estimated_tokens).collect();
    let part_tokens = if calibrated {
        allocate(&part_weights, tokens)
    } else {
        part_weights
    };
    let mut items: Vec<_> = analysis
        .items
        .iter()
        .map(|item| PresentedItem {
            item: item.clone(),
            tokens: item.estimated_tokens,
        })
        .collect();
    if calibrated {
        for (part, &total) in summary.parts.iter().zip(&part_tokens) {
            let indexes: Vec<_> = items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.item.kind == part.kind)
                .map(|(index, _)| index)
                .collect();
            let weights: Vec<_> = indexes
                .iter()
                .map(|index| items[*index].item.estimated_tokens)
                .collect();
            for (index, value) in indexes.into_iter().zip(allocate(&weights, total)) {
                items[index].tokens = value;
            }
        }
    }
    ContextPresentation {
        schema_version: 1,
        content_bytes: summary.content_bytes,
        estimated_tokens: summary.estimated_tokens,
        tokens,
        calibrated,
        reported_input_tokens: reported,
        coverage: summary.coverage.clone(),
        parts: summary
            .parts
            .iter()
            .cloned()
            .zip(part_tokens)
            .map(|(part, tokens)| PresentedPart { part, tokens })
            .collect(),
        items,
    }
}
