//! Read-only interpretation of captured apply_patch arguments. This module never
//! resolves paths, reads a workspace, applies a patch, or infers execution status.
use serde::Serialize;

const MAX_FILES: usize = 64;
const MAX_SECTIONS: usize = 256;
const MAX_ROWS: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProposedPatch {
    Ready {
        environment_id: Option<String>,
        files: Vec<ProposedFile>,
    },
    Unavailable {
        reason: PatchIssue,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchIssue {
    Incomplete,
    Truncated,
    IdentityConflict,
    UnsupportedFormat,
    Budget,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Add,
    Update,
    Delete,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Added,
    Removed,
    Context,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchLine {
    pub kind: LineKind,
    pub text: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchSection {
    pub anchor: Option<String>,
    pub at_eof: bool,
    pub lines: Vec<PatchLine>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProposedFile {
    pub operation: Operation,
    pub path: String,
    pub move_to: Option<String>,
    pub sections: Vec<PatchSection>,
    pub added_lines: u32,
    /// Delete File carries no old contents, so its removed line count is unknown.
    pub removed_lines: Option<u32>,
}
impl ProposedPatch {
    pub fn bytes(&self) -> usize {
        match self {
            Self::Unavailable { .. } => 32,
            Self::Ready {
                environment_id,
                files,
            } => {
                environment_id.as_ref().map_or(0, String::len)
                    + files
                        .iter()
                        .map(|file| {
                            256 + file.path.len()
                                + file.move_to.as_ref().map_or(0, String::len)
                                + file
                                    .sections
                                    .iter()
                                    .map(|section| {
                                        64 + section.anchor.as_ref().map_or(0, String::len)
                                            + section
                                                .lines
                                                .iter()
                                                .map(|line| line.text.len() + 64)
                                                .sum::<usize>()
                                    })
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            }
        }
    }
}

pub fn preview(safe_arguments: &str) -> ProposedPatch {
    match parse(safe_arguments) {
        Ok((environment_id, files)) => ProposedPatch::Ready {
            environment_id,
            files,
        },
        Err(reason) => ProposedPatch::Unavailable { reason },
    }
}
fn path(value: &str) -> Result<String, PatchIssue> {
    if value.len() > 4096 {
        return Err(PatchIssue::Budget);
    }
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(PatchIssue::UnsupportedFormat);
    }
    Ok(value.to_owned())
}
fn finished(file: &ProposedFile) -> bool {
    file.operation != Operation::Update
        || (!file.sections.is_empty()
            && file
                .sections
                .iter()
                .all(|section| !section.lines.is_empty()))
}
fn parse(raw: &str) -> Result<(Option<String>, Vec<ProposedFile>), PatchIssue> {
    if raw.len() > super::tool::PREVIEW_BYTES {
        return Err(PatchIssue::Budget);
    }
    let mut lines: Vec<_> = raw.trim().lines().take(4097).collect();
    if lines.len() > 4096 {
        return Err(PatchIssue::Budget);
    }
    // Accept the official parser's common literal heredoc wrapper, not a shell
    // command containing a patch or code which happens to mention its markers.
    if matches!(lines.first(), Some(&"<<EOF" | &"<<'EOF'" | &"<<\"EOF\""))
        && lines.last() == Some(&"EOF")
        && lines.len() >= 4
    {
        lines = lines[1..lines.len() - 1].to_vec();
    }
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch")
        || lines.last().map(|line| line.trim()) != Some("*** End Patch")
    {
        return Err(PatchIssue::UnsupportedFormat);
    }
    let mut files: Vec<ProposedFile> = Vec::new();
    let (mut environment_id, mut sections, mut rows) = (None, 0, 0);
    for line in &lines[1..lines.len() - 1] {
        // In an update a leading space is a context-line marker, including for
        // text resembling a file header. Do not trim it away.
        let header = if files
            .last()
            .is_some_and(|file| file.operation == Operation::Update)
        {
            line.trim_end()
        } else {
            line.trim()
        };
        if files.is_empty()
            && let Some(id) = header.strip_prefix("*** Environment ID: ")
        {
            if environment_id.is_some()
                || id.trim().is_empty()
                || id.len() > 128
                || id.chars().any(char::is_control)
            {
                return Err(PatchIssue::UnsupportedFormat);
            }
            environment_id = Some(id.trim().to_owned());
            continue;
        }
        let next = [
            ("*** Add File: ", Operation::Add),
            ("*** Delete File: ", Operation::Delete),
            ("*** Update File: ", Operation::Update),
        ]
        .into_iter()
        .find_map(|(prefix, operation)| header.strip_prefix(prefix).map(|name| (operation, name)));
        if let Some((operation, name)) = next {
            if files.len() >= MAX_FILES {
                return Err(PatchIssue::Budget);
            }
            if files.last().is_some_and(|file| !finished(file)) {
                return Err(PatchIssue::UnsupportedFormat);
            }
            files.push(ProposedFile {
                operation,
                path: path(name)?,
                move_to: None,
                sections: Vec::new(),
                added_lines: 0,
                removed_lines: (operation != Operation::Delete).then_some(0),
            });
            continue;
        }
        let file = files.last_mut().ok_or(PatchIssue::UnsupportedFormat)?;
        if file.operation == Operation::Update {
            if file.sections.is_empty()
                && file.move_to.is_none()
                && let Some(name) = header.strip_prefix("*** Move to: ")
            {
                file.move_to = Some(path(name)?);
                continue;
            }
            let start = header == "@@" || header.starts_with("@@ ");
            if file.sections.last().is_some_and(|section| section.at_eof) {
                if header.is_empty() {
                    continue;
                }
                if !start {
                    return Err(PatchIssue::UnsupportedFormat);
                }
            }
            if start {
                if file
                    .sections
                    .last()
                    .is_some_and(|section| section.lines.is_empty())
                {
                    return Err(PatchIssue::UnsupportedFormat);
                }
                let anchor = header.strip_prefix("@@ ").map(str::to_owned);
                sections += 1;
                if sections > MAX_SECTIONS {
                    return Err(PatchIssue::Budget);
                }
                file.sections.push(PatchSection {
                    anchor,
                    ..PatchSection::default()
                });
                continue;
            }
            if header == "*** End of File" {
                let section = file
                    .sections
                    .last_mut()
                    .filter(|section| !section.lines.is_empty())
                    .ok_or(PatchIssue::UnsupportedFormat)?;
                section.at_eof = true;
                continue;
            }
        }
        let (kind, text) = match (file.operation, line.as_bytes().first()) {
            (Operation::Add | Operation::Update, Some(b'+')) => (LineKind::Added, &line[1..]),
            (Operation::Update, Some(b'-')) => (LineKind::Removed, &line[1..]),
            (Operation::Update, Some(b' ')) => (LineKind::Context, &line[1..]),
            (Operation::Update, None) => (LineKind::Context, ""),
            _ => return Err(PatchIssue::UnsupportedFormat),
        };
        rows += 1;
        if rows > MAX_ROWS {
            return Err(PatchIssue::Budget);
        }
        if file.sections.is_empty() {
            sections += 1;
            if sections > MAX_SECTIONS {
                return Err(PatchIssue::Budget);
            }
            file.sections.push(PatchSection::default());
        }
        file.added_lines += u32::from(kind == LineKind::Added);
        if kind == LineKind::Removed {
            *file.removed_lines.as_mut().unwrap() += 1;
        }
        file.sections.last_mut().unwrap().lines.push(PatchLine {
            kind,
            text: text.to_owned(),
        });
    }
    if files.last().is_some_and(|file| !finished(file)) {
        return Err(PatchIssue::UnsupportedFormat);
    }
    Ok((environment_id, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn files(raw: &str) -> Vec<ProposedFile> {
        match preview(raw) {
            ProposedPatch::Ready { files, .. } => files,
            _ => panic!("fixture patch should have a structured preview"),
        }
    }
    #[test]
    fn add_update_delete_move_and_eof_preserve_patch_lines_without_inventing_file_positions() {
        let parsed = files(
            "*** Begin Patch\n*** Environment ID: synthetic-env\n*** Add File: 新文件.txt\n+new\n+\n*** Update File: old.txt\n*** Move to: moved.txt\n@@ anchor\n keep  \n-same\n+same\n@@\n-tail\n+next\n*** End of File\n\n*** Delete File: deleted.txt\n*** End Patch",
        );
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].path, "新文件.txt");
        assert_eq!(parsed[0].added_lines, 2);
        assert_eq!(parsed[1].move_to.as_deref(), Some("moved.txt"));
        assert_eq!(parsed[1].sections[0].anchor.as_deref(), Some("anchor"));
        assert_eq!(parsed[1].sections[0].lines[0].text, "keep  ");
        assert_eq!(parsed[1].added_lines, 2);
        assert_eq!(parsed[1].removed_lines, Some(2));
        assert!(parsed[1].sections[1].at_eof);
        assert_eq!(parsed[2].removed_lines, None);
    }
    #[test]
    fn context_markers_literal_html_and_paths_are_reading_data_not_executable_actions() {
        let parsed = files(
            "*** Begin Patch\n*** Update File: ../<img>.txt\n *** Delete File: literal.txt\n-<svg onload=alert(1)>\n+<script>literal</script>\n*** End Patch",
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path, "../<img>.txt");
        assert_eq!(parsed[0].sections[0].lines[0].kind, LineKind::Context);
        assert_eq!(
            parsed[0].sections[0].lines[0].text,
            "*** Delete File: literal.txt"
        );
        assert_eq!(
            parsed[0].sections[0].lines[2].text,
            "<script>literal</script>"
        );
        assert_eq!(
            files("<<'EOF'\n*** Begin Patch\n*** Add File: empty.txt\n*** End Patch\nEOF")[0]
                .added_lines,
            0
        );
    }
    #[test]
    fn partial_invalid_shell_code_and_unknown_variants_have_no_plausible_partial_diff() {
        for raw in [
            "",
            "*** Begin Patch\n*** Add File: file\n+partial",
            "*** Begin Patch\n*** Update File: file\n@@\n*** End Patch",
            "*** Begin Patch\n*** Delete File: file\n-old\n*** End Patch",
            "*** Begin Patch\n*** Update File: file\n*** Move to: other\n*** End Patch",
            "*** Begin Patch\n*** Unknown File: file\n*** End Patch",
            "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: file\n+data\n*** End Patch\nEOF",
            "text(await tools.apply_patch('*** Begin Patch'));",
            "*** Begin Patch\n*** Add File: \n+data\n*** End Patch",
            "*** Begin Patch\n*** Update File: f\n@@\n+data\n*** End of File\n+unexpected\n*** End Patch",
        ] {
            assert_eq!(
                preview(raw),
                ProposedPatch::Unavailable {
                    reason: PatchIssue::UnsupportedFormat
                }
            );
        }
    }
    #[test]
    fn budgets_bound_many_small_lines_and_files_even_when_arguments_fit_the_text_budget() {
        let many_lines = format!(
            "*** Begin Patch\n*** Add File: file\n{}*** End Patch",
            "+x\n".repeat(2001)
        );
        assert_eq!(
            preview(&many_lines),
            ProposedPatch::Unavailable {
                reason: PatchIssue::Budget
            }
        );
        let many_files = format!(
            "*** Begin Patch\n{}*** End Patch",
            "*** Delete File: file\n".repeat(65)
        );
        assert_eq!(
            preview(&many_files),
            ProposedPatch::Unavailable {
                reason: PatchIssue::Budget
            }
        );
    }
}
