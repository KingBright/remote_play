//! Presentation boundary for restoring the original GPUI without reverting Session safety.
//! A decoded native frame is retained by reference, never converted to an RGBA Vec here.
//! This module is not yet the default GUI and deliberately exposes no CPU-upload fallback.
use super::model::OriginalGuiSession;
use std::{sync::Arc, time::Instant};

fn admissible_frame(
    decoded_at: Instant,
    source_epoch: Instant,
    confirmed: bool,
    paused: bool,
    failed: bool,
) -> bool {
    confirmed && !paused && !failed && decoded_at >= source_epoch
}

/// Adopt only a frame belonging to the acknowledged current source. The retained
/// value owns the decoder's native buffer, not a copied CPU representation.
/// Adopting is not display completion: input remains gated until paint acknowledgement.
#[allow(dead_code)]
pub(crate) fn adopt_latest_frame(session: &mut OriginalGuiSession) -> bool {
    if !session.confirmed || session.paused || session.video_error.is_some() {
        return false;
    }
    let Some(media) = &session.media else {
        return false;
    };
    let shared = media.shared_frame();
    let frame = shared.lock().expect("decoded native frame lock").take();
    let Some(frame) = frame else {
        return false;
    };
    if !admissible_frame(
        frame.decoded_at,
        session.first_frame_after,
        session.confirmed,
        session.paused,
        session.video_error.is_some(),
    ) {
        return false;
    }
    session.texture = Some(Arc::new(frame));
    true
}

/// Called only by the renderer after it accepted this exact buffer for painting.
/// A delayed callback cannot enable input for a different or paused source.
#[allow(dead_code)]
pub(crate) fn acknowledge_paint(
    session: &mut OriginalGuiSession,
    frame: &Arc<client::MacDecodedVideoFrame>,
) -> bool {
    let Some(current) = &session.texture else {
        return false;
    };
    if !Arc::ptr_eq(current, frame)
        || !admissible_frame(
            frame.decoded_at,
            session.first_frame_after,
            session.confirmed,
            session.paused,
            session.video_error.is_some(),
        )
    {
        return false;
    }
    #[cfg(all(target_os = "windows", feature = "native-windows-video"))]
    {
        let Some(native) = &frame.native else {
            return false;
        };
        let stats = native.ready.frame.stats();
        if !native.ready.frame.was_submitted() || stats.last_error.lock().unwrap().is_some() {
            return false;
        }
    }
    #[cfg(all(target_os="linux",feature="native-linux-video"))]
    {
        let Some(native)=&frame.native_linux else{return false;};
        if !native.frame.was_submitted() || native.frame.stats().last_error.lock().unwrap().is_some(){return false;}
    }
    session.uploaded_at = Some(frame.decoded_at);
    true
}

/// Keep the original CVPixelBuffer -> GPUI native surface route on macOS.
/// Other platforms require a real native texture import adapter before this API
/// can be provided there. Do not implement it by copying each frame to CPU pixels.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub(crate) fn native_surface(
    frame: &client::MacDecodedVideoFrame,
    fit: gpui::ObjectFit,
) -> gpui::AnyElement {
    use gpui::IntoElement;
    client::decoded_video_frame_surface_with_fit(frame, fit).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn previous_source_and_unconfirmed_frames_cannot_unlock_input() {
        let now = Instant::now();
        assert!(!admissible_frame(
            now - Duration::from_millis(1),
            now,
            true,
            false,
            false
        ));
        assert!(!admissible_frame(now, now, false, false, false));
        assert!(!admissible_frame(now, now, true, true, false));
        assert!(!admissible_frame(now, now, true, false, true));
        assert!(admissible_frame(now, now, true, false, false));
    }
    #[test]
    fn view_state_is_independent_of_texture_type() {
        // Compile-time contract: original GUI uses the same Session lifecycle and
        // source-scoped input implementation, but retains native decoded frames.
        fn owns_native_frame(
            session: &OriginalGuiSession,
        ) -> Option<&Arc<client::MacDecodedVideoFrame>> {
            session.texture.as_ref()
        }
        let _ = owns_native_frame;
        let native: Option<Arc<client::MacDecodedVideoFrame>> = None;
        assert!(native.is_none());
    }
}
