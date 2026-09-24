//! Source probes are off the HTTP executor; no production sources are invented.
use super::*;
use crate::history::legacy_reader::{Collection, LegacyReader, Query, ReadBudget};
use std::sync::RwLock;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceStatus {
    pub id: String,
    pub kind: &'static str,
    pub state: &'static str,
    pub message: String,
}
pub(crate) struct LegacySource {
    pub id: String,
    pub database: PathBuf,
    pub blobs: PathBuf,
}
pub(crate) type SourceState = Arc<RwLock<Vec<SourceStatus>>>;
pub(super) struct SourceWorker {
    state: SourceState,
    budget: ReadBudget,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl SourceWorker {
    pub fn start(home: PathBuf, data: PathBuf, sources: Vec<LegacySource>) -> Result<Self> {
        ensure!(sources.len() <= 8, "source limit exceeded");
        let state = Arc::new(RwLock::new(vec![SourceStatus {
            id: "native".into(),
            kind: "native",
            state: "checking",
            message: "正在检查原生历史位置…".into(),
        }]));
        let output = state.clone();
        let budget = ReadBudget::default();
        let cancelled = budget.clone();
        let thread = std::thread::Builder::new()
            .name("history-source-probe".into())
            .spawn(move || {
                let mut result = vec![
                    SourceStatus {
                        id: "native".into(),
                        kind: "native",
                        state: if home.is_dir() {
                            "not_indexed"
                        } else {
                            "unavailable"
                        },
                        message: if home.is_dir() {
                            "已找到原生历史位置；跨项目目录将在下一阶段接入。"
                        } else {
                            "未找到 Codex 原生目录。历史首页仍可使用，启动对话前请先配置 Codex。"
                        }
                        .into(),
                    },
                    SourceStatus {
                        id: "workbench".into(),
                        kind: "workbench",
                        state: "not_indexed",
                        message: if data.exists() {
                            "已找到工作台历史位置；跨项目目录尚未接入。"
                        } else {
                            "尚无工作台历史目录；开始对话后会按需保存。"
                        }
                        .into(),
                    },
                ];
                if sources.is_empty() {
                    result.push(SourceStatus {
                        id: "observer".into(),
                        kind: "observer",
                        state: "not_configured",
                        message: "尚未登记旧历史来源；来源接入将在下一阶段提供。".into(),
                    });
                }
                for source in sources {
                    // Reader has its own per-operation deadline and shares cancellation.
                    let result_page = LegacyReader::open(
                        &source.id,
                        &source.database,
                        Some(&source.blobs),
                        &cancelled,
                    )
                    .and_then(|reader| {
                        reader.page(
                            &Query {
                                collection: Collection::Projects,
                                scope: None,
                            },
                            None,
                            20,
                            &cancelled,
                        )
                    });
                    let (status, message) = match result_page {
                        Ok(page) => (
                            "readable",
                            format!(
                                "旧历史可只读访问；首批读取 {} 个项目，完整目录尚未接入。",
                                page.records.len()
                            ),
                        ),
                        Err(error) => (
                            "unavailable",
                            format!("旧历史暂不可用（{}）；源文件未修改。", error.code),
                        ),
                    };
                    result.push(SourceStatus {
                        id: source.id,
                        kind: "observer",
                        state: status,
                        message,
                    });
                }
                *output.write().unwrap() = result;
            })?;
        Ok(Self {
            state,
            budget,
            thread: Some(thread),
        })
    }
    pub fn state(&self) -> SourceState {
        self.state.clone()
    }
}
impl Drop for SourceWorker {
    fn drop(&mut self) {
        self.budget.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
