//! Bounded aliases proven by an event carrying both output_index and item ID.
//! Keep the first presentation key so streamed content and DOM state stay put.
use super::DiagnosticCode;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ContentKind {
    Text,
    Function,
    Custom,
}
impl From<super::tool::ToolKind> for ContentKind {
    fn from(kind: super::tool::ToolKind) -> Self {
        match kind {
            super::tool::ToolKind::Function => Self::Function,
            super::tool::ToolKind::Custom => Self::Custom,
        }
    }
}

struct Binding {
    canonical: String,
    wire: Option<String>,
    output_index: Option<u32>,
    kind: ContentKind,
}

#[derive(Default)]
pub(super) struct ItemIdentities {
    bindings: Vec<Binding>,
    by_wire: HashMap<(Option<String>, String), usize>,
    by_index: HashMap<(Option<String>, u32), usize>,
}
impl ItemIdentities {
    pub fn bytes(&self) -> usize {
        self.bindings
            .iter()
            .map(|item| item.canonical.len() + item.wire.as_ref().map_or(0, String::len) + 128)
            .sum::<usize>()
            + self
                .by_wire
                .keys()
                .map(|(response, wire)| response.as_ref().map_or(0, String::len) + wire.len() + 64)
                .sum::<usize>()
            + self
                .by_index
                .keys()
                .map(|(response, _)| response.as_ref().map_or(0, String::len) + 64)
                .sum::<usize>()
    }

    pub fn resolve(
        &mut self,
        response: Option<String>,
        wire: Option<String>,
        output_index: Option<u32>,
        kind: ContentKind,
        limit: usize,
    ) -> Result<String, DiagnosticCode> {
        let by_wire = wire
            .as_ref()
            .and_then(|wire| self.by_wire.get(&(response.clone(), wire.clone())))
            .copied();
        let by_index = output_index
            .and_then(|index| self.by_index.get(&(response.clone(), index)))
            .copied();
        if by_wire
            .zip(by_index)
            .is_some_and(|(left, right)| left != right)
        {
            // Two already-published items cannot be silently combined on an
            // ambiguous late bridge. Retain both and report the conflict.
            return Err(DiagnosticCode::ConflictingIdentity);
        }
        let index = if let Some(index) = by_wire.or(by_index) {
            let binding = &self.bindings[index];
            if binding.kind != kind
                || binding
                    .wire
                    .as_ref()
                    .zip(wire.as_ref())
                    .is_some_and(|(old, new)| old != new)
                || binding
                    .output_index
                    .zip(output_index)
                    .is_some_and(|(old, new)| old != new)
            {
                return Err(DiagnosticCode::ConflictingIdentity);
            }
            index
        } else {
            let canonical = wire
                .clone()
                .or_else(|| output_index.map(|index| format!("@output:{index}")))
                .ok_or(DiagnosticCode::MissingIdentity)?;
            if self.bindings.len() >= limit {
                return Err(DiagnosticCode::Capacity);
            }
            self.bindings.push(Binding {
                canonical,
                wire: None,
                output_index: None,
                kind,
            });
            self.bindings.len() - 1
        };
        let binding = &mut self.bindings[index];
        if let Some(wire) = wire {
            binding.wire = Some(wire.clone());
            self.by_wire.insert((response.clone(), wire), index);
        }
        if let Some(output_index) = output_index {
            binding.output_index = Some(output_index);
            self.by_index.insert((response, output_index), index);
        }
        Ok(binding.canonical.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_require_the_same_response_slot_and_kind_and_remain_bounded() {
        let mut identities = ItemIdentities::default();
        let r = Some("response".to_owned());
        let key = identities
            .resolve(r.clone(), None, Some(0), ContentKind::Text, 4)
            .unwrap();
        assert_eq!(
            identities
                .resolve(
                    r.clone(),
                    Some("wire".into()),
                    Some(0),
                    ContentKind::Text,
                    4
                )
                .unwrap(),
            key
        );
        assert_eq!(
            identities
                .resolve(r.clone(), Some("wire".into()), None, ContentKind::Text, 4)
                .unwrap(),
            key
        );
        for (wire, index, kind) in [
            ("other", 0, ContentKind::Text),
            ("wire", 1, ContentKind::Text),
            ("wire", 0, ContentKind::Function),
        ] {
            assert_eq!(
                identities.resolve(r.clone(), Some(wire.into()), Some(index), kind, 4),
                Err(DiagnosticCode::ConflictingIdentity)
            );
        }
        let other_response = Some("other-response".into());
        assert_eq!(
            identities
                .resolve(
                    other_response,
                    Some("wire".into()),
                    Some(0),
                    ContentKind::Function,
                    4
                )
                .unwrap(),
            "wire"
        );
        identities
            .resolve(
                r.clone(),
                Some("independent".into()),
                None,
                ContentKind::Text,
                4,
            )
            .unwrap();
        identities
            .resolve(r.clone(), None, Some(3), ContentKind::Text, 4)
            .unwrap();
        assert_eq!(
            identities.resolve(
                r.clone(),
                Some("independent".into()),
                Some(3),
                ContentKind::Text,
                4
            ),
            Err(DiagnosticCode::ConflictingIdentity)
        );
        assert_eq!(
            identities.resolve(r, Some("overflow".into()), Some(4), ContentKind::Text, 4),
            Err(DiagnosticCode::Capacity)
        );
        assert!(identities.bytes() < 4096);
    }
}
