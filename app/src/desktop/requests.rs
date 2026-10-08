//! Per-connection shared-file request correlation, independent of rendering.
use protocol::shared_files::SharedFileResponse;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    List { after: u64 },
    Fetch { file: u64 },
}
#[derive(Default)]
pub struct FileRequests {
    next: u64,
    pending: BTreeMap<u64, (RequestKind, Instant)>,
}
impl FileRequests {
    pub fn begin(&mut self, kind: RequestKind, now: Instant) -> Result<u64, &'static str> {
        if self.pending.len() >= 32 {
            return Err("Too many pending file requests; wait or retry");
        }
        if matches!(kind, RequestKind::List { .. }) && self.list_pending() {
            return Err("A file list request is already pending");
        }
        if self.pending.values().any(|(k, _)| *k == kind) {
            return Err("This file request is already pending");
        }
        loop {
            self.next = self.next.wrapping_add(1).max(1);
            if !self.pending.contains_key(&self.next) {
                break;
            }
        }
        self.pending.insert(self.next, (kind, now));
        Ok(self.next)
    }
    pub fn cancel(&mut self, id: u64) {
        self.pending.remove(&id);
    }
    pub fn list_pending(&self) -> bool {
        self.pending
            .values()
            .any(|(k, _)| matches!(k, RequestKind::List { .. }))
    }
    pub fn resolve(&mut self, id: u64, response: &SharedFileResponse) -> Option<RequestKind> {
        let kind = self.pending.get(&id)?.0;
        let matches = matches!(
            (kind, response),
            (RequestKind::List { .. }, SharedFileResponse::Page { .. })
                | (RequestKind::Fetch { .. }, SharedFileResponse::Queued { .. })
                | (_, SharedFileResponse::Rejected { .. })
        );
        if !matches {
            return None;
        }
        self.pending.remove(&id);
        Some(kind)
    }
    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.pending.len();
        self.pending
            .retain(|_, (_, at)| now.saturating_duration_since(*at) < Duration::from_secs(10));
        before - self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> SharedFileResponse {
        SharedFileResponse::Page {
            entries: vec![],
            next_after_id: None,
        }
    }
    fn rejected() -> SharedFileResponse {
        SharedFileResponse::Rejected {
            message: "test".into(),
        }
    }
    #[test]
    fn refresh_and_fetch_never_reuse_the_file_id_as_request_id() {
        let mut r = FileRequests::default();
        let now = Instant::now();
        let a = r.begin(RequestKind::Fetch { file: u64::MAX }, now).unwrap();
        r.resolve(a, &rejected());
        let b = r.begin(RequestKind::Fetch { file: u64::MAX }, now).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, u64::MAX);
    }
    #[test]
    fn late_list_reply_does_not_override_current_request() {
        let mut r = FileRequests::default();
        let now = Instant::now();
        let a = r.begin(RequestKind::List { after: 0 }, now).unwrap();
        assert_eq!(r.expire(now + Duration::from_secs(11)), 1);
        let b = r
            .begin(
                RequestKind::List { after: 99 },
                now + Duration::from_secs(11),
            )
            .unwrap();
        assert!(r.resolve(a, &page()).is_none());
        assert!(r.list_pending());
        assert_eq!(r.resolve(b, &page()), Some(RequestKind::List { after: 99 }));
        assert!(!r.list_pending());
    }
    #[test]
    fn fetch_reply_cannot_finish_a_file_list() {
        let mut r = FileRequests::default();
        let now = Instant::now();
        let list = r.begin(RequestKind::List { after: 0 }, now).unwrap();
        let fetch = r.begin(RequestKind::Fetch { file: 8 }, now).unwrap();
        assert!(r.resolve(fetch, &page()).is_none());
        assert_eq!(
            r.resolve(fetch, &rejected()),
            Some(RequestKind::Fetch { file: 8 })
        );
        assert!(r.list_pending());
        assert!(r.resolve(list, &page()).is_some());
    }
    #[test]
    fn queue_rejection_is_undoable_and_request_state_is_bounded() {
        let mut r = FileRequests::default();
        let now = Instant::now();
        let a = r.begin(RequestKind::List { after: 0 }, now).unwrap();
        r.cancel(a);
        assert!(!r.list_pending());
        for file in 0..32 {
            r.begin(RequestKind::Fetch { file }, now).unwrap();
        }
        assert!(r.begin(RequestKind::Fetch { file: 32 }, now).is_err());
        assert_eq!(r.expire(now + Duration::from_secs(10)), 32);
    }
    #[test]
    fn duplicate_click_is_deduplicated_only_until_the_reply() {
        let mut r = FileRequests::default();
        let now = Instant::now();
        let a = r.begin(RequestKind::Fetch { file: 3 }, now).unwrap();
        assert!(r.begin(RequestKind::Fetch { file: 3 }, now).is_err());
        r.resolve(a, &rejected());
        assert!(r.begin(RequestKind::Fetch { file: 3 }, now).is_ok());
    }
}
