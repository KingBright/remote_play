//! The existing background owner wakes its same-profile frontend and dispatches
//! to the private GUI adapter. No network/media work runs on the GUI thread.
use crate::desktop::directed_receive::{Request, exchange};
use host::directed_delivery::{Incoming, LEASE};
use protocol::session::{DeliveryClaimProof, DeliveryState};
use std::{path::PathBuf, sync::Arc, time::Duration};
type Error = Box<dyn std::error::Error + Send + Sync>;
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
    mut rx: tokio::sync::mpsc::Receiver<Arc<Incoming>>,
) {
    while let Some(offer) = rx.recv().await {
        let _end = EndOffer(offer.clone());
        if !offer.alive() {
            continue;
        }
        if let Err(error) = receive(&profile, &hash, &offer).await {
            offer.finish(DeliveryState::Rejected, &error.to_string());
        }
    }
}
async fn receive(
    profile: &std::path::Path,
    hash: &str,
    offer: &Arc<Incoming>,
) -> Result<(), Error> {
    let directory = profile.join("gui-state");
    let mut status = ipc(directory.clone(), Request::Status).await;
    if status.is_err() {
        // Only absence permits activation. Ownership/permissions and version
        // mismatches must never be treated as a missing GUI.
        crate::background_service::private_directory(profile)?;
        let missing = match directory.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
            Ok(_) => {
                crate::background_service::private_directory(&directory)?;
                let socket = directory.join("gui-receive.sock");
                match socket.symlink_metadata() {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                    Err(error) => return Err(error.into()),
                    Ok(_) => {
                        crate::background_service::private_socket(&socket)?;
                        std::os::unix::net::UnixStream::connect(socket)
                            .err()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
                    }
                }
            }
        };
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
            .env(remote_core::mesh::REMOTE_PLAY_DEVICE_GROUP_DIR_ENV, profile)
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
        let mut applied = 1;
        while offer.alive() {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (revision, expires, _) = offer.snapshot();
            if !offer.alive() {
                break;
            }
            let status = ipc(directory.clone(), Request::Status).await?;
            if status["generation"].as_str() != Some(&generation)
                || status["lease"]["request_id"].as_str() != Some(&request_id)
            {
                return Err("Receiver stopped or changed generation".into());
            }
            if receiving_status(&status, &generation, &request_id, &offer.source) {
                offer.finish(DeliveryState::Receiving, "");
            }
            let failed = status["sessions"]["sessions"]
                .as_array()
                .is_some_and(|sessions| {
                    sessions.iter().any(|s| {
                        s["device_id"].as_str() == Some(&offer.source)
                            && (!s["error"].is_null() || !s["video_error"].is_null())
                    })
                });
            if status["sessions"]["directed_receive_failed"] == true {
                return Err("Receiver could not connect to the source".into());
            }
            if failed {
                return Err("Receiver could not start video".into());
            }
            if revision > applied {
                let lease_ms = expires
                    .saturating_duration_since(std::time::Instant::now())
                    .min(LEASE)
                    .as_millis() as u64;
                if lease_ms < 1_000 {
                    break;
                }
                let response = ipc(
                    directory.clone(),
                    Request::Renew {
                        generation: generation.clone(),
                        request_id: request_id.clone(),
                        deadline_ms: crate::unix_now_ms() + 5_000,
                        revision,
                        lease_ms,
                    },
                )
                .await?;
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

fn receiving_status(
    status: &serde_json::Value,
    generation: &str,
    request: &str,
    source: &str,
) -> bool {
    status["generation"].as_str() == Some(generation)
        && status["lease"]["request_id"].as_str() == Some(request)
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
}
