use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BlobRecord {
    pub blob_id: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub path: PathBuf,
}

pub(super) struct PreparedBlob {
    pub(super) blob_id: String,
    pub(super) stored_hash: String,
    pub(super) media_type: &'static str,
    pub(super) size_bytes: usize,
    pub(super) relative_path: String,
    pub(super) redaction_json: String,
}
