//! The existing background owner wakes its same-profile frontend and dispatches
//! to the private GUI adapter. No network/media work runs on the GUI thread.
use crate::desktop::directed_receive::{Request, exchange};
use host::directed_delivery::{Incoming, LEASE};
use protocol::session::{DeliveryClaimProof, DeliveryState};
use std::{
    future::Future,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error + Send + Sync>;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_FRAME_TIMEOUT_REASON: &str =
    "Receiver did not report its first submitted video frame within 30 seconds";

/// A renewable sending intent must not keep a failed startup alive forever.
/// Once this exact receive has submitted a decoded frame, an unchanged desktop
/// is allowed to remain quiet for the rest of its renewable lease.
struct FirstFrameDeadline {
    deadline: Option<Instant>,
}
impl FirstFrameDeadline {
    fn new(now: Instant) -> Self {
        Self {
            deadline: Some(now + FIRST_FRAME_TIMEOUT),
        }
    }
    async fn wait<F: Future>(&self, work: F) -> Result<F::Output, &'static str> {
        match self.deadline {
            Some(deadline) => tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), work)
                .await
                .map_err(|_| FIRST_FRAME_TIMEOUT_REASON),
            None => Ok(work.await),
        }
    }
    fn observe(
        &mut self,
        status: &serde_json::Value,
        generation: &str,
        request: &str,
        source: &str,
        now: Instant,
    ) -> Result<bool, &'static str> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(FIRST_FRAME_TIMEOUT_REASON);
        }
        let receiving = receiving_status(status, generation, request, source);
        if receiving {
            self.deadline = None;
        }
        Ok(receiving)
    }
}
async fn ipc(directory: PathBuf, request: Request) -> Result<serde_json::Value, Error> {
    tokio::task::spawn_blocking(move || exchange(&directory, &request)).await?
}
struct EndOffer(Arc<Incoming>);
impl Drop for EndOffer {
    fn drop(&mut self) {
        self.0
            .finish(DeliveryState::Cancelled, "Receiver worker ended");
    }
}
pub(crate) async fn receive_worker(
    profile: PathBuf,
    hash: String,
    owner_mode: crate::background_service::OwnerMode,
    mut rx: tokio::sync::mpsc::Receiver<Arc<Incoming>>,
) {
    while let Some(offer) = rx.recv().await {
        let _end = EndOffer(offer.clone());
        if !offer.alive() {
            continue;
        }
        if let Err(error) = receive(&profile, &hash, owner_mode, &offer).await {
            offer.finish(DeliveryState::Rejected, &error.to_string());
        }
    }
}
async fn receive(
    profile: &std::path::Path,
    hash: &str,
    owner_mode: crate::background_service::OwnerMode,
    offer: &Arc<Incoming>,
) -> Result<(), Error> {
    let directory = if owner_mode == crate::background_service::OwnerMode::Background { profile.join("gui-state") } else { profile.to_owned() };
    let mut status = ipc(directory.clone(), Request::Status).await;
    if status.is_err() {
        // Only absence permits activation. Ownership/permissions and version
        // mismatches must never be treated as a missing GUI.
        crate::background_service::private_directory(profile)?;
        if owner_mode != crate::background_service::OwnerMode::Background { return Err("The running device window has no available receive adapter".into()); }
        let missing = crate::private_ipc::absent(&directory, "gui-receive.sock")?;
        if !missing {
            return Err("Existing GUI endpoint is unavailable".into());
        }
        if crate::background_service::executable_hash()? != hash {
            return Err("Installed executable changed; restart the service before delivery".into());
        }
        if !offer.alive() {
            return Err("Sending intent expired".into());
        }
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .arg("--gui")
            .env(remote_core::mesh::WEAVELO_DEVICE_GROUP_DIR_ENV, profile)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        // Reap only this normal frontend launch; never stop an existing GUI.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while status.is_err() && tokio::time::Instant::now() < deadline && offer.alive() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            status = ipc(directory.clone(), Request::Status).await;
        }
    }
    let status = status?;
    if status["ok"] != true
        || status["executable_sha256"].as_str() != Some(hash)
        || status["renderer"].as_str() != Some(crate::gui_backend::ORIGINAL_RENDERER)
    {
        return Err("Receiver GUI executable or renderer mismatch".into());
    }
    let generation = status["generation"]
        .as_str()
        .ok_or("Missing GUI generation")?
        .to_owned();
    let request_id = format!(
        "push-{}-{}-{:02x}{:02x}{:02x}{:02x}",
        offer.peer.connection,
        offer.request,
        offer.peer.nonce[0],
        offer.peer.nonce[1],
        offer.peer.nonce[2],
        offer.peer.nonce[3]
    );
    if !offer.alive() {
        return Err("Sending intent expired".into());
    }
    let (_, expires, _) = offer.snapshot();
    let lease_ms = expires
        .saturating_duration_since(std::time::Instant::now())
        .as_millis() as u64;
    if lease_ms < 1_000 {
        return Err("Sending intent expired".into());
    }
    let response = ipc(
        directory.clone(),
        Request::Receive {
            generation: generation.clone(),
            request_id: request_id.clone(),
            source_device_id: offer.source.clone(),
            deadline_ms: crate::unix_now_ms() + 5_000,
            lease_ms,
            claim: Some(DeliveryClaimProof {
                ticket: offer.ticket,
                target_device_id: offer.target.clone(),
                request_id: offer.request,
            }),
        },
    )
    .await;
    let result = async {
        let response = response?;
        if response["ok"] != true {
            return Err(response["error"]
                .as_str()
                .unwrap_or("Receiver busy")
                .to_owned()
                .into());
        }
        if !offer.alive() {
            return Err("Sending intent ended during GUI activation".into());
        }
        offer.finish(DeliveryState::Accepted, "");
        let mut first_frame = FirstFrameDeadline::new(Instant::now());
        let mut applied = 1;
        while offer.alive() {
            first_frame.wait(tokio::time::sleep(Duration::from_millis(300))).await?;
            let (revision, expires, _) = offer.snapshot();
            if !offer.alive() {
                break;
            }
            let status = first_frame.wait(ipc(directory.clone(), Request::Status)).await??;
            if status["generation"].as_str() != Some(&generation)
                || status["lease"]["request_id"].as_str() != Some(&request_id)
            {
                return Err("Receiver stopped or changed generation".into());
            }
            if first_frame.observe(&status, &generation, &request_id, &offer.source, Instant::now())? {
                offer.finish(DeliveryState::Receiving, "");
            }
            if receiving_failed(&status, &generation, &request_id, &offer.source) {
                return Err("Receiver could not connect or continue video".into());
            }
            if revision > applied {
                let lease_ms = expires
                    .saturating_duration_since(std::time::Instant::now())
                    .min(LEASE)
                    .as_millis() as u64;
                if lease_ms < 1_000 {
                    break;
                }
                let response = first_frame.wait(ipc(
                    directory.clone(),
                    Request::Renew {
                        generation: generation.clone(),
                        request_id: request_id.clone(),
                        deadline_ms: crate::unix_now_ms() + 5_000,
                        revision,
                        lease_ms,
                    },
                ))
                .await??;
                if response["ok"] != true {
                    return Err("Receiver lease ended".into());
                }
                applied = revision;
            }
        }
        Ok::<(), Error>(())
    }
    .await;
    let _ = ipc(
        directory,
        Request::Stop {
            generation,
            request_id,
            deadline_ms: crate::unix_now_ms() + 5_000,
        },
    )
    .await;
    result
}

fn receiving_failed(status:&serde_json::Value,generation:&str,request:&str,source:&str)->bool {
    if status["generation"].as_str()!=Some(generation)||status["lease"]["request_id"].as_str()!=Some(request){return false;}
    status["sessions"]["directed_receive_failed"]==true || status["sessions"]["sessions"].as_array().is_some_and(|sessions|sessions.iter().any(|s|{
        s["device_id"].as_str()==Some(source) && (s["connected"]==false || s["video_error"].as_str().is_some_and(|error|!error.trim().is_empty()))
    }))
}
fn receiving_status(
    status: &serde_json::Value,
    generation: &str,
    request: &str,
    source: &str,
) -> bool {
    status["generation"].as_str() == Some(generation)
        && status["lease"]["request_id"].as_str() == Some(request)
        && !receiving_failed(status,generation,request,source)
        && has_submitted_video(status, source)
}
fn has_submitted_video(status: &serde_json::Value, source: &str) -> bool {
    status["sessions"]["sessions"]
        .as_array()
        .is_some_and(|sessions| {
            sessions.iter().any(|s| {
                s["device_id"].as_str() == Some(source)
                    && s["connected"] == true
                    && s["decoded_frames"].as_u64().unwrap_or(0) > 0
                    && s["scene_submitted_frame"] == true
            })
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn confirmed_receiver() -> serde_json::Value {
        serde_json::json!({"generation":"gui","lease":{"request_id":"request","remaining_ms":15_000},
            "sessions":{"directed_receive_failed":false,"sessions":[{"device_id":"source","connected":true,
                "peer_responsive":true,"confirmed":true,"error":"","video_error":null,
                "decoded_frames":0,"scene_submitted_frame":false}]}})
    }
    #[test]
    fn confirmed_healthy_receiver_without_video_expires_despite_renewals() {
        let start = Instant::now();
        let mut deadline = FirstFrameDeadline::new(start);
        let mut status = confirmed_receiver();
        for second in 0..30 {
            // Both the authenticated connection and renewable intent remain
            // healthy; neither demonstrates that the first picture arrived.
            status["lease"]["remaining_ms"] = 15_000.into();
            assert!(!receiving_failed(&status, "gui", "request", "source"));
            assert!(!deadline.observe(&status, "gui", "request", "source", start + Duration::from_secs(second)).unwrap());
        }
        assert_eq!(deadline.observe(&status, "gui", "request", "source", start + FIRST_FRAME_TIMEOUT), Err(FIRST_FRAME_TIMEOUT_REASON));
        status["sessions"]["sessions"][0]["decoded_frames"] = 1.into();
        status["sessions"]["sessions"][0]["scene_submitted_frame"] = true.into();
        assert_eq!(deadline.observe(&status, "gui", "request", "source", start + FIRST_FRAME_TIMEOUT), Err(FIRST_FRAME_TIMEOUT_REASON));
    }
    #[test]
    fn only_exact_receive_first_frame_disarms_deadline_and_static_picture_stays_valid() {
        let start = Instant::now();
        let mut deadline = FirstFrameDeadline::new(start);
        let mut status = confirmed_receiver();
        status["sessions"]["sessions"][0]["decoded_frames"] = 1.into();
        assert!(!deadline.observe(&status, "gui", "request", "source", start).unwrap());
        status["sessions"]["sessions"][0]["scene_submitted_frame"] = true.into();
        for (generation, request, source) in [
            ("old-gui", "request", "source"),
            ("gui", "old-request", "source"),
            ("gui", "request", "other-source"),
        ] {
            let mut unrelated = FirstFrameDeadline::new(start);
            assert!(!unrelated.observe(&status, generation, request, source, start).unwrap());
            assert_eq!(unrelated.observe(&status, generation, request, source, start + FIRST_FRAME_TIMEOUT), Err(FIRST_FRAME_TIMEOUT_REASON));
        }
        assert!(deadline.observe(&status, "gui", "request", "source", start + Duration::from_secs(1)).unwrap());
        // No additional frame is required after the first submitted picture.
        assert!(deadline.observe(&status, "gui", "request", "source", start + Duration::from_secs(86_400)).unwrap());
    }
    #[tokio::test]
    async fn first_frame_budget_also_bounds_a_stalled_status_exchange() {
        let deadline = FirstFrameDeadline { deadline: Some(Instant::now() + Duration::from_millis(10)) };
        assert_eq!(deadline.wait(std::future::pending::<()>()).await, Err(FIRST_FRAME_TIMEOUT_REASON));
    }
    #[test]
    fn receiving_requires_same_source_decoded_and_submitted_frame() {
        let mut status = serde_json::json!({"sessions":{"sessions":[{"device_id":"source","connected":true,"decoded_frames":0,"scene_submitted_frame":false}]}});
        status["generation"] = "gui-1".into();
        status["lease"] = serde_json::json!({"request_id":"request-1"});
        assert!(!receiving_status(&status, "gui-1", "request-1", "source"));
        assert!(!has_submitted_video(&status, "source"));
        status["sessions"]["sessions"][0]["decoded_frames"] = 1.into();
        assert!(!has_submitted_video(&status, "source"));
        status["sessions"]["sessions"][0]["scene_submitted_frame"] = true.into();
        assert!(has_submitted_video(&status, "source"));
        assert!(receiving_status(&status, "gui-1", "request-1", "source"));
        assert!(!receiving_status(&status, "gui-2", "request-1", "source"));
        assert!(!receiving_status(&status, "gui-1", "request-2", "source"));
        assert!(!has_submitted_video(&status, "different-source"));
        status["sessions"]["sessions"][0]["connected"] = false.into();
        assert!(!has_submitted_video(&status, "source"));
    }
    #[test]
    fn normal_status_and_audio_warning_do_not_end_video_delivery() {
        let mut status=serde_json::json!({"generation":"gui","lease":{"request_id":"r"},"sessions":{"directed_receive_failed":false,"sessions":[{"device_id":"source","connected":true,"error":"","video_error":null,"decoded_frames":1,"scene_submitted_frame":true}]}});
        assert!(!receiving_failed(&status,"gui","r","source"));assert!(receiving_status(&status,"gui","r","source"));
        status["sessions"]["sessions"][0]["error"]="Audio unavailable: device disconnected".into();
        assert!(!receiving_failed(&status,"gui","r","source"));assert!(receiving_status(&status,"gui","r","source"));
        status["sessions"]["sessions"][0]["video_error"]="decoder failed".into();
        assert!(receiving_failed(&status,"gui","r","source"));assert!(!receiving_status(&status,"gui","r","source"));
        assert!(!receiving_failed(&status,"gui","r","other-source"));assert!(!receiving_failed(&status,"gui","old-request","source"));
        status["sessions"]["sessions"][0]["video_error"]=serde_json::Value::Null;status["sessions"]["sessions"][0]["connected"]=false.into();
        assert!(receiving_failed(&status,"gui","r","source"));
    }

}
