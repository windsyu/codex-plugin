use super::*;

impl Reader {
    fn git(
        &self,
        args: &[&str],
        files: &[std::fs::File],
        budget: &Budget,
    ) -> Result<process::Output> {
        let program = self.programs.git.as_ref().ok_or(Fault("git_unavailable"))?;
        let mut fixed: Vec<String> = [
            "--no-pager",
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "protocol.allow=never",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "diff.external=",
            "-c",
            "diff.noprefix=false",
            "-c",
            "log.showSignature=false",
            "-c",
            "core.quotePath=true",
            "-c",
            "color.ui=false",
            "-c",
            "submodule.recurse=false",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        fixed.extend(args.iter().map(|s| s.to_string()));
        process::run(&self.root, program, &fixed, files, budget, FILE_LIMIT)
    }
    fn git_ok(&self, args: &[&str], budget: &Budget) -> Result<Vec<u8>> {
        let out = self.git(args, &[], budget)?;
        if out.truncated {
            return Err(Fault("git_output_limit"));
        }
        if out.code != Some(0) {
            return Err(Fault("git_read_failed"));
        }
        Ok(out.bytes)
    }
    fn repository(&self, budget: &Budget) -> Result<String> {
        let output = self.git(&["rev-parse", "--is-inside-work-tree"], &[], budget)?;
        if output.code != Some(0) || output.bytes != b"true\n" {
            return Err(Fault("not_git_repository"));
        }
        let prefix = self.git_ok(&["rev-parse", "--show-prefix"], budget)?;
        let prefix = String::from_utf8(prefix).map_err(|_| Fault("unsupported_path"))?;
        Ok(prefix.strip_suffix('\n').unwrap_or(&prefix).to_string())
    }
    fn head(&self, budget: &Budget) -> Result<Option<String>> {
        let out = self.git(&["rev-parse", "--verify", "--quiet", "HEAD"], &[], budget)?;
        if out.code == Some(1) {
            return Ok(None);
        }
        if out.code != Some(0) || out.truncated {
            return Err(Fault("git_read_failed"));
        }
        let hash = String::from_utf8_lossy(&out.bytes).trim().to_string();
        if !object_id(&hash) {
            return Err(Fault("git_read_failed"));
        }
        Ok(Some(hash))
    }
    pub(super) fn git_status(&self, budget: &Budget) -> Result<Value> {
        let prefix = self.repository(budget)?;
        let branch = self.git(&["symbolic-ref", "--quiet", "--short", "HEAD"], &[], budget)?;
        let branch = (branch.code == Some(0)).then(|| {
            String::from_utf8_lossy(&branch.bytes)
                .trim_end_matches('\n')
                .to_string()
        });
        let output = self.git(
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
                "--",
                ".",
            ],
            &[],
            budget,
        )?;
        if !output.truncated && output.code != Some(0) {
            return Err(Fault("git_read_failed"));
        }
        let mut records = output.bytes.split_inclusive(|b| *b == 0);
        let mut entries = Vec::new();
        let mut omitted = 0;
        while let Some(record) = records.next() {
            if record.last() != Some(&0) || record.len() < 5 {
                continue;
            }
            let rename = matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C');
            let original = if rename { records.next() } else { None };
            let Some(path) = std::str::from_utf8(&record[3..record.len() - 1])
                .ok()
                .and_then(|p| p.strip_prefix(&prefix))
            else {
                omitted += 1;
                continue;
            };
            if self.root.git_path(path).is_err() {
                omitted += 1;
                continue;
            }
            let original = original
                .and_then(|r| r.strip_suffix(&[0]))
                .and_then(|r| std::str::from_utf8(r).ok())
                .and_then(|p| p.strip_prefix(&prefix))
                .filter(|p| fs::validate(p, false).is_ok());
            entries.push(json!({"path":path,"index":(record[0] as char).to_string(),"working":(record[1] as char).to_string(),"originalPath":original}));
        }
        Ok(
            json!({"branch":branch,"entries":entries,"omitted":omitted,"truncated":output.truncated}),
        )
    }
    pub(super) fn git_log(&self, cursor: Option<&str>, budget: &Budget) -> Result<Value> {
        let prefix = self.repository(budget)?;
        let Some(head) = self.head(budget)? else {
            return Ok(json!({"commits":[],"nextCursor":null,"truncated":false}));
        };
        let fingerprint = blake3::hash(format!("{head}:{prefix}").as_bytes())
            .to_hex()
            .to_string();
        let offset = fs::page_offset(cursor, &fingerprint, 20)?;
        let skip = format!("--skip={offset}");
        let out = self.git(
            &[
                "log",
                "--no-show-signature",
                "--no-decorate",
                "--no-notes",
                "--format=%H%x00%h%x00%an%x00%aI%x00%s%x00",
                "--max-count=21",
                &skip,
                &head,
                "--",
                ".",
            ],
            &[],
            budget,
        )?;
        if !out.truncated && out.code != Some(0) {
            return Err(Fault("git_read_failed"));
        }
        let mut commits = Vec::new();
        for record in out.bytes.split(|b| *b == b'\n') {
            let parts: Vec<_> = record.split(|b| *b == 0).collect();
            if parts.len() != 6 {
                continue;
            }
            let parts: Vec<_> = parts
                .iter()
                .map(|p| String::from_utf8_lossy(p).to_string())
                .collect();
            if !object_id(&parts[0]) {
                continue;
            }
            commits.push(json!({"id":parts[0],"shortId":parts[1],"author":parts[2],"date":parts[3],"subject":parts[4]}));
        }
        let next = (commits.len() > 20).then(|| format!("{fingerprint}:{}", offset + 20));
        commits.truncate(20);
        Ok(json!({"commits":commits,"nextCursor":next,"truncated":out.truncated}))
    }
    fn blob(
        &self,
        path: &str,
        head: Option<&str>,
        index: bool,
        budget: &Budget,
    ) -> Result<Vec<u8>> {
        let listing = if index {
            self.git_ok(&["ls-files", "--stage", "-z", "--", path], budget)?
        } else if let Some(head) = head {
            self.git_ok(&["ls-tree", "-z", head, "--", path], budget)?
        } else {
            return Ok(Vec::new());
        };
        if listing.is_empty() {
            return Ok(Vec::new());
        }
        let records: Vec<_> = listing
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
            .collect();
        if records.len() != 1 {
            return Err(Fault("git_conflict"));
        }
        let (meta, listed_path) = records[0].split_at(
            records[0]
                .iter()
                .position(|b| *b == b'\t')
                .ok_or(Fault("git_read_failed"))?,
        );
        if &listed_path[1..] != path.as_bytes() {
            return Err(Fault("git_read_failed"));
        }
        let meta = std::str::from_utf8(meta).map_err(|_| Fault("git_read_failed"))?;
        let fields: Vec<_> = meta.split(' ').collect();
        if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") {
            return Err(Fault("forbidden_path"));
        }
        if index && fields[2] != "0" {
            return Err(Fault("git_conflict"));
        }
        let id = if index { fields[1] } else { fields[2] };
        if !object_id(id) {
            return Err(Fault("git_read_failed"));
        }
        let size = self.git_ok(&["cat-file", "-s", id], budget)?;
        let size: usize = std::str::from_utf8(&size)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or(Fault("git_read_failed"))?;
        if size > FILE_LIMIT {
            return Err(Fault("file_too_large"));
        }
        self.git_ok(&["cat-file", "blob", id], budget)
    }
    pub(super) fn git_diff(&self, path: &str, staged: bool, budget: &Budget) -> Result<Value> {
        self.root.git_path(path)?;
        self.repository(budget)?;
        let head = self.head(budget)?;
        let before = if staged {
            self.blob(path, head.as_deref(), false, budget)?
        } else {
            self.blob(path, None, true, budget)?
        };
        let after = if staged {
            self.blob(path, None, true, budget)?
        } else {
            match self.root.read(path) {
                Ok(s) => s.into_bytes(),
                Err(Fault("not_found")) => Vec::new(),
                Err(e) => return Err(e),
            }
        };
        let before = fs::text(before)?;
        let after = fs::text(after)?;
        let fingerprint = blake3::hash(format!("{before}\0{after}").as_bytes())
            .to_hex()
            .to_string();
        let files = [
            process::snapshot(before.as_bytes())?,
            process::snapshot(after.as_bytes())?,
        ];
        let output = self.git(
            &[
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-renames",
                "--",
                "/dev/fd/64",
                "/dev/fd/65",
            ],
            &files,
            budget,
        )?;
        if !output.truncated && !matches!(output.code, Some(0 | 1)) {
            return Err(Fault("git_read_failed"));
        }
        // Strip descriptor headers; the UI labels the explicitly selected path.
        let raw = String::from_utf8_lossy(&output.bytes);
        let patch = raw
            .find("@@ ")
            .map(|start| raw[start..].to_string())
            .unwrap_or_default();
        Ok(
            json!({"path":path,"scope":if staged {"staged"} else {"working"},"patch":patch,"revision":fingerprint,"truncated":output.truncated,"comparison":"file_contents","empty":before==after}),
        )
    }
}
fn object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
