//! Versioned connection and independent media subscription control.
use serde::{Deserialize, Serialize};

pub const SESSION_VERSION: u32 = 2;
pub const MAX_SUBSCRIPTIONS: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum CaptureSource {
    #[default]
    MainDisplay,
    Display(u32),
    Window(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureSourceInfo {
    pub source: CaptureSource,
    pub title: String,
    pub application: String,
    pub process_id: Option<i32>,
    pub width: u32,
    pub height: u32,
    pub supports_input: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubscriptionRequest {
    pub id: u32,
    pub source: CaptureSource,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionCommand {
    ReleaseInput {
        id: u32,
    },
    InputReleased {
        id: u32,
    },
    Configure {
        id: u32,
        revision: u64,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    },
    Configured {
        id: u32,
        revision: u64,
    },
    Closed {
        connection_id: u32,
        reason: String,
    },
    SetClipboard {
        enabled: bool,
    },
    ClipboardState {
        enabled: bool,
    },
    Open {
        connection_id: u32,
        version: u32,
    },
    Opened {
        connection_id: u32,
        version: u32,
        max_subscriptions: u32,
        files: bool,
        clipboard: bool,
        window_capture: bool,
    },
    ListSources {
        request_id: u32,
    },
    Sources {
        request_id: u32,
        sources: Vec<CaptureSourceInfo>,
    },
    Subscribe(SubscriptionRequest),
    Subscribed {
        id: u32,
        audio_owner: Option<u32>,
        supports_input: bool,
    },
    Input {
        id: u32,
        event: crate::InputEvent,
    },
    SetActivity {
        id: u32,
        revision: u64,
        video: bool,
        audio: bool,
    },
    Activity {
        id: u32,
        revision: u64,
        video: bool,
        audio: bool,
    },
    Unsubscribe {
        id: u32,
    },
    Unsubscribed {
        id: u32,
    },
    Close {
        connection_id: u32,
    },
    Error {
        request_id: u32,
        reason: String,
    },
    // Appended variants preserve existing serialized enum discriminants.
    // A source switch has its own request identity, separate from media/session IDs.
    SwitchSource {
        id: u32,
        request_id: u32,
        source: CaptureSource,
    },
    SourceSwitched {
        id: u32,
        request_id: u32,
        source: CaptureSource,
        supports_input: bool,
    },
    // Window input is bound to a source revision. An old datagram cannot act on
    // the newly selected window even when the media subscription ID is reused.
    SourceInput {
        id: u32,
        source_revision: u32,
        event: crate::InputEvent,
    },
    RequestInputBinding { connection_id: u32, id: u32, source_revision: u32, request: u64 },
    InputBinding { binding: crate::realtime_input::InputBinding, request: u64, host_time_ms: u64 },
    RealtimeInput(crate::realtime_input::RealtimeInput),
    InputHandled { binding: crate::realtime_input::InputBinding, sequence: u64 },
    // Directed delivery uses this same authenticated control connection. The
    // opaque ticket is an intent capability, never a new device identity.
    DeliveryOffer { request_id: u64, source_device_id: String, target_device_id: String, ticket: [u8; 32], revision: u64 },
    DeliveryClaim { request_id: u64, target_device_id: String, ticket: [u8; 32] },
    DeliveryCancel { request_id: u64 },
    DeliveryReceipt { request_id: u64, revision: u64, state: DeliveryState, reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryClaimProof { pub ticket:[u8;32], pub target_device_id:String, pub request_id:u64 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeliveryState { Pending, Accepted, Claimed, Cancelled, Rejected, Receiving }

/// Fit inside both requested dimensions, preserve source aspect and align 4:2:0
/// output. Rounding is bounded to one pixel per dimension, never stretch to 16:9.
pub fn fit_capture_size(source: (u32, u32), target: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (source.0.max(2), source.1.max(2));
    let scale = (target.0.max(2) as f64 / sw as f64)
        .min(target.1.max(2) as f64 / sh as f64)
        .min(1.0);
    (
        ((sw as f64 * scale) as u32 & !1).max(2),
        ((sh as f64 * scale) as u32 & !1).max(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fits_both_axes_for_portrait_landscape_and_odd_sizes() {
        assert_eq!(fit_capture_size((1080, 1920), (1920, 1080)), (606, 1080));
        assert_eq!(fit_capture_size((3440, 1440), (1280, 720)), (1280, 534));
        assert_eq!(fit_capture_size((913, 913), (1920, 1080)), (912, 912));
        assert_eq!(fit_capture_size((0, 0), (0, 0)), (2, 2));
    }
    #[test]
    fn delivery_commands_round_trip_without_changing_existing_wire_indices(){
        let existing=[(SessionCommand::ReleaseInput{id:7},0u32),(SessionCommand::Open{connection_id:9,version:SESSION_VERSION},7),
            (SessionCommand::Close{connection_id:9},18),(SessionCommand::RequestInputBinding{connection_id:9,id:7,source_revision:0,request:1},23)];
        for(command,index)in existing {let bytes=bincode::serialize(&command).unwrap();assert_eq!(&bytes[..4],&index.to_le_bytes());}
        let commands=[SessionCommand::DeliveryOffer{request_id:17,source_device_id:"source".into(),target_device_id:"target".into(),ticket:[3;32],revision:1},
            SessionCommand::DeliveryClaim{request_id:17,target_device_id:"target".into(),ticket:[3;32]},
            SessionCommand::DeliveryCancel{request_id:17},
            SessionCommand::DeliveryReceipt{request_id:17,revision:2,state:DeliveryState::Receiving,reason:String::new()}];
        for(index,command)in commands.into_iter().enumerate(){
            let bytes=bincode::serialize(&command).unwrap();assert_eq!(&bytes[..4],&(27u32+index as u32).to_le_bytes());
            let decoded:SessionCommand=bincode::deserialize(&bytes).unwrap();assert_eq!(bincode::serialize(&decoded).unwrap(),bytes);
        }
    }

}
