//! Optional, bounded pull requests carried on the existing authenticated file lane.
use serde::{Deserialize, Serialize};
pub const SHARED_PAGE_SIZE: usize = 8;
pub const MAX_SHARED_NAME_BYTES: usize = 128;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedFileInfo {
    pub id: u64,
    pub name: String,
    pub size_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedFileRequest {
    List { after_id: u64 },
    Fetch { file_id: u64 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SharedFileResponse {
    Page {
        entries: Vec<SharedFileInfo>,
        next_after_id: Option<u64>,
    },
    Queued {
        transfer_id: u64,
    },
    Rejected {
        message: String,
    },
}
