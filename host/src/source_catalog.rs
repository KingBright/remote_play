//! Source enumeration is slow native work, never a control-loop wait.
use protocol::session::CaptureSourceInfo;
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinHandle, time::Instant};

pub(crate) type CatalogResult = Result<Vec<CaptureSourceInfo>, String>;
#[derive(Default)]
pub(crate) struct SourceCatalog {
    pending: Option<(u32, JoinHandle<CatalogResult>)>,
    recent: Option<(u32, Instant, CatalogResult)>,
}
impl SourceCatalog {
    // A retry of the same request joins the existing query. Different requests
    // get an explicit busy response; the queue cannot grow with UDP retries.
    pub(crate) fn request(&mut self, id: u32) -> Option<CatalogResult> {
        self.request_with(id, || {
            crate::capture_sources::list().map_err(|e| e.to_string())
        })
    }
    fn request_with(
        &mut self,
        id: u32,
        enumerate: impl FnOnce() -> CatalogResult + Send + 'static,
    ) -> Option<CatalogResult> {
        if let Some((pending, _)) = &self.pending {
            return (*pending != id).then(|| Err("source enumeration is busy".into()));
        }
        if let Some((previous, at, result)) = &self.recent
            && *previous == id
            && at.elapsed() < Duration::from_secs(1)
        {
            return Some(result.clone());
        }
        // One native enumeration at a time across peers, including queries
        // whose connection closed while the non-cancellable native call ran.
        static LIMIT: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let limit = LIMIT.get_or_init(|| Arc::new(Semaphore::new(1))).clone();
        let task = tokio::spawn(async move {
            let permit = limit.acquire_owned().await.map_err(|e| e.to_string())?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let mut result = enumerate();
                if let Ok(sources) = &mut result {
                    sources.truncate(256);
                }
                result
            })
            .await
            .map_err(|e| e.to_string())?
        });
        self.pending = Some((id, task));
        None
    }
    pub(crate) async fn take_ready(&mut self) -> Option<(u32, CatalogResult)> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|(_, task)| task.is_finished())
        {
            return None;
        }
        let (id, task) = self.pending.take().unwrap();
        let result = task.await.unwrap_or_else(|e| Err(e.to_string()));
        self.recent = Some((id, Instant::now(), result.clone()));
        Some((id, result))
    }
}
impl Drop for SourceCatalog {
    fn drop(&mut self) {
        if let Some((_, task)) = &self.pending {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    #[tokio::test]
    async fn slow_catalog_does_not_hold_control_and_udp_retries_do_not_repeat_native_work() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let mut query = SourceCatalog::default();
        assert!(
            query
                .request_with(7, move || {
                    count.fetch_add(1, SeqCst);
                    let _ = started.send(());
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(Vec::new())
                })
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap();
        for _ in 0..20 {
            assert!(
                query
                    .request_with(7, || panic!("UDP retry re-enumerated"))
                    .is_none()
            );
        }
        assert!(
            query
                .request_with(8, || panic!("unbounded queue"))
                .unwrap()
                .is_err()
        );
        // A control-loop poll returns while the native query remains blocked;
        // no timing-dependent assertion against the native platform is needed.
        assert!(query.take_ready().await.is_none());
        assert_eq!(calls.load(SeqCst), 1);
        release.send(()).unwrap();
        let (id, result) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(reply) = query.take_ready().await {
                    break reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(id, 7);
        assert!(result.is_ok());
        assert!(
            query
                .request_with(7, || panic!("queued duplicate re-enumerated"))
                .unwrap()
                .is_ok()
        );
        assert!(
            query.request_with(8, || Ok(Vec::new())).is_none(),
            "new request must refresh"
        );
    }
    #[tokio::test]
    async fn closed_connection_cannot_receive_or_rebind_old_catalog_result() {
        let mut old = SourceCatalog::default();
        assert!(old.request_with(7, || Ok(Vec::new())).is_none());
        drop(old);
        let mut replacement = SourceCatalog::default();
        assert!(replacement.take_ready().await.is_none());
        assert!(
            replacement
                .request_with(7, || Err("new owner".into()))
                .is_none()
        );
        let (_, result) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(reply) = replacement.take_ready().await {
                    break reply;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err(), "new owner");
    }
}
