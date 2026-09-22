use super::*;
use std::collections::BTreeSet;

impl Reader {
    fn rg(
        &self,
        args: &[String],
        files: &[std::fs::File],
        budget: &Budget,
    ) -> Result<process::Output> {
        let program = self.programs.rg.as_ref().ok_or(Fault("rg_unavailable"))?;
        process::run(&self.root, program, args, files, budget, FILE_LIMIT)
    }
    pub(super) fn search(
        &self,
        query: &str,
        regex: bool,
        case: bool,
        budget: &Budget,
    ) -> Result<Value> {
        if query.is_empty() || query.len() > 4096 || query.contains(['\0', '\n', '\r']) {
            return Err(Fault("invalid_search"));
        }
        let mut search_args = vec![
            "--no-config",
            "--json",
            "--no-mmap",
            "--color=never",
            "--max-count=201",
            "--regex-size-limit=1M",
            "--dfa-size-limit=1M",
        ];
        if !regex {
            search_args.push("--fixed-strings");
        }
        search_args.push(if case {
            "--case-sensitive"
        } else {
            "--ignore-case"
        });
        search_args.push("-e");
        let mut args: Vec<String> = search_args.into_iter().map(String::from).collect();
        args.push(query.into());
        args.push("--".into());
        // Validate the expression even in an empty/ignored workspace.
        let mut probe = args.clone();
        probe.push("/dev/fd/64".into());
        let validation = self.rg(&probe, &[process::snapshot(b"")?], budget)?;
        if !matches!(validation.code, Some(0 | 1)) {
            return Err(Fault("invalid_regex"));
        }
        let listing = self.rg(
            &[
                "--no-config",
                "--files",
                "--hidden",
                "--no-ignore-global",
                "--no-ignore-parent",
                "--null",
                "--",
                ".",
            ]
            .map(String::from),
            &[],
            budget,
        )?;
        if !listing.truncated && !matches!(listing.code, Some(0 | 1)) {
            return Err(Fault("search_failed"));
        }
        let mut names = BTreeSet::new();
        let mut omitted = 0;
        for part in listing.bytes.split_inclusive(|b| *b == 0) {
            if part.last() != Some(&0) {
                continue;
            }
            let Ok(path) = std::str::from_utf8(&part[..part.len() - 1]) else {
                omitted += 1;
                continue;
            };
            let path = path.strip_prefix("./").unwrap_or(path);
            if fs::validate(path, false).is_ok() {
                names.insert(path.to_string());
            } else {
                omitted += 1;
            }
        }
        let mut reason = listing.truncated.then_some("file_list_limit");
        let mut hits = Vec::<Value>::new();
        let mut scanned = 0;
        let mut total = 0;
        let mut iterator = names.into_iter();
        'scan: loop {
            let mut files = Vec::new();
            let mut paths = Vec::new();
            for path in iterator.by_ref().take(16) {
                if budget.check().is_err() {
                    reason = Some("time_limit");
                    break 'scan;
                }
                let text = match self.root.read(&path) {
                    Ok(text) => text,
                    Err(_) => {
                        omitted += 1;
                        continue;
                    }
                };
                total += text.len();
                scanned += 1;
                if total > 32 * FILE_LIMIT || scanned > 2048 {
                    reason = Some("scan_limit");
                    break 'scan;
                }
                files.push(process::snapshot(text.as_bytes())?);
                paths.push(path);
            }
            if files.is_empty() {
                if iterator.len() == 0 {
                    break;
                }
                continue;
            }
            let mut command = args.clone();
            for i in 0..files.len() {
                command.push(format!("/dev/fd/{}", 64 + i));
            }
            let output = match self.rg(&command, &files, budget) {
                Ok(out) => out,
                Err(Fault("workspace_timeout")) => {
                    reason = Some("time_limit");
                    break;
                }
                Err(e) => return Err(e),
            };
            for line in output.bytes.split(|b| *b == b'\n') {
                let Ok(value) = serde_json::from_slice::<Value>(line) else {
                    continue;
                };
                if value["type"] != "match" {
                    continue;
                }
                let data = &value["data"];
                let Some(fd) = data["path"]["text"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("/dev/fd/"))
                    .and_then(|s| s.parse::<usize>().ok())
                else {
                    continue;
                };
                let Some(path) = fd.checked_sub(64).and_then(|n| paths.get(n)) else {
                    continue;
                };
                if hits.len() >= PAGE {
                    reason = Some("hit_limit");
                    break 'scan;
                }
                let content = data["lines"]["text"].as_str().unwrap_or("");
                let preview: String = content
                    .trim_end_matches(['\n', '\r'])
                    .chars()
                    .take(1000)
                    .collect();
                hits.push(json!({"path":path,"line":data["line_number"],"text":preview,"truncated":content.chars().count()>1000}));
            }
            if output.truncated {
                reason = Some("output_limit");
                break;
            }
            if !matches!(output.code, Some(0 | 1)) {
                return Err(Fault("search_failed"));
            }
            if iterator.len() == 0 {
                break;
            }
        }
        if budget.cancelled.load(Ordering::Relaxed) {
            return Err(Fault("cancelled"));
        }
        Ok(
            json!({"hits":hits,"scannedFiles":scanned,"omitted":omitted,"truncated":reason.is_some(),"limitReason":reason}),
        )
    }
}
