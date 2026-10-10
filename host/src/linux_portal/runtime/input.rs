//! Input belongs to the exact consented RemoteDesktop session and stream.
//! No system permissions, global injector, EIS connection or restore token.

use super::{Connection, OwnedObjectPath, REMOTE_DESKTOP, SelectedPortalSource, SourceKind, proxy};
use crate::linux_frame::{OwnedFrame, Transform};
use protocol::{InputEvent, input_modifiers};
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, watch};
use zbus::zvariant::{Type, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalInputError {
    ViewOnly,
    DevicePermission,
    WindowTarget,
    GeometryUnknown,
    FrameBinding,
    SessionClosed,
    InvalidEvent,
    Transport,
}
impl std::fmt::Display for PortalInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ViewOnly => "This source is view-only; select control locally through the normal RemoteDesktop dialog",
            Self::DevicePermission => "The local RemoteDesktop dialog did not grant both keyboard and pointer access",
            Self::WindowTarget => "This Wayland window has no verified input focus binding; desktop input is disabled",
            Self::GeometryUnknown => "The selected Linux stream has no verified logical geometry",
            Self::FrameBinding => "Linux stream geometry changed or has not been validated; select the source again for control",
            Self::SessionClosed => "The local Linux sharing session was cancelled or revoked",
            Self::InvalidEvent => "Input event is unsupported for this Linux source; no desktop fallback was used",
            Self::Transport => "Linux portal input failed; release and select the source again",
        })
    }
}
impl std::error::Error for PortalInputError {}
type Result<T> = std::result::Result<T, PortalInputError>;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameBinding {
    revision: u64,
    width: u32,
    height: u32,
}
fn frame_binding(frame: &OwnedFrame) -> Option<FrameBinding> {
    let format = frame.source_format;
    if format.transform != Transform::Identity
        || format.crop.is_some_and(|crop| {
            crop.x != 0 || crop.y != 0 || crop.width != format.width || crop.height != format.height
        })
        || frame.width == 0
        || frame.height == 0
    {
        return None;
    }
    Some(FrameBinding {
        revision: frame.stamp.format_revision,
        width: frame.width,
        height: frame.height,
    })
}

#[derive(Default)]
struct Held {
    keys: BTreeSet<i32>,
    buttons: BTreeSet<i32>,
}

pub(super) struct PortalInput {
    connection: Connection,
    destination: String,
    session: OwnedObjectPath,
    source: SelectedPortalSource,
    devices: u32,
    revoked: watch::Receiver<bool>,
    cancelled: watch::Receiver<bool>,
    binding: Mutex<Option<FrameBinding>>,
    valid: AtomicBool,
    failed: AtomicBool,
    held: AsyncMutex<Held>,
}
impl PortalInput {
    pub(super) fn new(
        connection: Connection,
        destination: String,
        session: OwnedObjectPath,
        source: SelectedPortalSource,
        devices: u32,
        revoked: watch::Receiver<bool>,
        cancelled: watch::Receiver<bool>,
    ) -> Self {
        Self {
            connection,
            destination,
            session,
            source,
            devices,
            revoked,
            cancelled,
            binding: Mutex::new(None),
            valid: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            held: AsyncMutex::new(Held::default()),
        }
    }
    pub(super) fn bind_frame(&self, frame: &OwnedFrame) {
        let mut binding = self.binding.lock().unwrap_or_else(|e| e.into_inner());
        if binding.is_none() && frame.stamp.generation == self.source.generation {
            *binding = frame_binding(frame);
            self.valid.store(binding.is_some(), Ordering::Release);
        }
    }
    pub(super) fn observe_frame(self: &std::sync::Arc<Self>, frame: &OwnedFrame) {
        let binding = *self.binding.lock().unwrap_or_else(|e| e.into_inner());
        if (frame.stamp.generation != self.source.generation || binding != frame_binding(frame))
            && self.valid.swap(false, Ordering::AcqRel)
        {
            // Start's logical geometry cannot be inferred from changed video pixels.
            // Keep video working, but require a new consented selection for input.
            let input = self.clone();
            tokio::spawn(async move {
                let _ = input.release().await;
            });
        }
    }
    pub(super) fn available(&self) -> bool {
        self.status().is_ok()
    }
    pub(super) fn status(&self) -> Result<()> {
        if *self.revoked.borrow()
            || *self.cancelled.borrow()
            || self.revoked.has_changed().is_err()
            || self.cancelled.has_changed().is_err()
        {
            return Err(PortalInputError::SessionClosed);
        }
        if self.devices != 3 {
            return Err(PortalInputError::DevicePermission);
        }
        if self.source.kind != SourceKind::Monitor {
            return Err(PortalInputError::WindowTarget);
        }
        if self.source.node_id == 0
            || self.source.node_id == u32::MAX
            || self
                .source
                .logical_size
                .is_none_or(|(w, h)| w <= 0 || h <= 0)
        {
            return Err(PortalInputError::GeometryUnknown);
        }
        if self.failed.load(Ordering::Acquire) {
            return Err(PortalInputError::Transport);
        }
        if !self.valid.load(Ordering::Acquire) {
            return Err(PortalInputError::FrameBinding);
        }
        Ok(())
    }
    async fn notify<B: Serialize + Type>(&self, name: &str, body: &B) -> Result<()> {
        tokio::time::timeout(Duration::from_millis(500), async {
            let portal = proxy(
                &self.connection,
                &self.destination,
                super::DESKTOP_PATH,
                REMOTE_DESKTOP,
            )
            .await
            .map_err(|_| PortalInputError::Transport)?;
            portal
                .call::<_, _, ()>(name, body)
                .await
                .map_err(|_| PortalInputError::Transport)
        })
        .await
        .map_err(|_| PortalInputError::Transport)?
    }
    async fn key(&self, held: &mut Held, key: i32, pressed: bool) -> Result<()> {
        // Record before RPC: a missing reply does not prove the compositor did
        // not inject the down event. A later release must include that key.
        if pressed {
            self.status()?;
            held.keys.insert(key);
        }
        self.notify(
            "NotifyKeyboardKeycode",
            &(&self.session, options(), key, u32::from(pressed)),
        )
        .await?;
        if !pressed {
            held.keys.remove(&key);
        }
        Ok(())
    }
    async fn button(&self, held: &mut Held, button: i32, pressed: bool) -> Result<()> {
        if pressed {
            self.status()?;
            held.buttons.insert(button);
        }
        self.notify(
            "NotifyPointerButton",
            &(&self.session, options(), button, u32::from(pressed)),
        )
        .await?;
        if !pressed {
            held.buttons.remove(&button);
        }
        Ok(())
    }
    async fn modifiers(&self, held: &mut Held, flags: u8) -> Result<()> {
        if flags
            & !(input_modifiers::SHIFT
                | input_modifiers::CONTROL
                | input_modifiers::ALT
                | input_modifiers::META
                | input_modifiers::CAPS_LOCK)
            != 0
        {
            return Err(PortalInputError::InvalidEvent);
        }
        // Caps Lock is a toggle key, not a held modifier; only its explicit key
        // event may toggle it. Do not infer the host's lock state from a viewer.
        for (flag, code) in [
            (input_modifiers::SHIFT, 42),
            (input_modifiers::CONTROL, 29),
            (input_modifiers::ALT, 56),
            (input_modifiers::META, 125),
        ] {
            let pressed = flags & flag != 0;
            if held.keys.contains(&code) != pressed {
                self.key(held, code, pressed).await?;
            }
        }
        Ok(())
    }
    pub(super) async fn inject(&self, event: InputEvent) -> Result<()> {
        let mut held = self.held.lock().await;
        self.status()?;
        let result = async {
            match event {
                InputEvent::MouseMoveAbsolute { x, y } => {
                    let (width, height) = self
                        .source
                        .logical_size
                        .ok_or(PortalInputError::GeometryUnknown)?;
                    // Session-local stream coordinates: no monitor offset, video
                    // pixel size, guessed scale, or raw node from a remote peer.
                    let x = f64::from(x) * f64::from(width - 1) / f64::from(u16::MAX);
                    let y = f64::from(y) * f64::from(height - 1) / f64::from(u16::MAX);
                    self.notify(
                        "NotifyPointerMotionAbsolute",
                        &(&self.session, options(), self.source.node_id, x, y),
                    )
                    .await
                }
                InputEvent::MouseDown(button) => {
                    self.button(&mut held, button_code(button)?, true).await
                }
                InputEvent::MouseUp(button) => {
                    self.button(&mut held, button_code(button)?, false).await
                }
                InputEvent::MouseScroll { delta_x, delta_y } => {
                    self.notify(
                        "NotifyPointerAxis",
                        &(
                            &self.session,
                            options(),
                            f64::from(delta_x),
                            f64::from(delta_y),
                        ),
                    )
                    .await
                }
                InputEvent::KeyDown(code) => {
                    self.key(
                        &mut held,
                        key_code(u16::try_from(code).map_err(|_| PortalInputError::InvalidEvent)?)?,
                        true,
                    )
                    .await
                }
                InputEvent::KeyUp(code) => {
                    self.key(
                        &mut held,
                        key_code(u16::try_from(code).map_err(|_| PortalInputError::InvalidEvent)?)?,
                        false,
                    )
                    .await
                }
                InputEvent::Key {
                    key_code: code,
                    pressed,
                    modifiers,
                } => {
                    let code = key_code(code)?;
                    self.modifiers(&mut held, modifiers).await?;
                    self.key(&mut held, code, pressed).await
                }
                InputEvent::ModifiersChanged(flags) => self.modifiers(&mut held, flags).await,
                InputEvent::MouseMove { .. } | InputEvent::Touch { .. } => {
                    Err(PortalInputError::InvalidEvent)
                }
            }
        }
        .await;
        if result == Err(PortalInputError::Transport) {
            self.failed.store(true, Ordering::Release);
            self.valid.store(false, Ordering::Release);
            drop(held);
            let _ = self.release().await;
        }
        result
    }
    pub(super) async fn release(&self) -> Result<()> {
        let result = tokio::time::timeout(Duration::from_millis(750), self.release_inner())
            .await
            .unwrap_or(Err(PortalInputError::Transport));
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        result
    }
    async fn release_inner(&self) -> Result<()> {
        let mut held = self.held.lock().await;
        let mut result = Ok(());
        for key in held.keys.clone() {
            if self.key(&mut held, key, false).await.is_err() {
                result = Err(PortalInputError::Transport);
            }
        }
        for button in held.buttons.clone() {
            if self.button(&mut held, button, false).await.is_err() {
                result = Err(PortalInputError::Transport);
            }
        }
        result
    }
}
fn options() -> std::collections::HashMap<&'static str, Value<'static>> {
    std::collections::HashMap::new()
}
fn button_code(button: u8) -> Result<i32> {
    Ok(match button {
        0 => 0x110,
        1 => 0x111,
        2 => 0x112,
        3 => 0x113,
        4 => 0x114,
        _ => return Err(PortalInputError::InvalidEvent),
    })
}
/// The existing wire key convention is macOS virtual keys, including GPUI on
/// Linux/Windows. Unknown values must not become another printable key.
fn key_code(code: u16) -> Result<i32> {
    Ok(match code {
        0x00 => 30,
        0x01 => 31,
        0x02 => 32,
        0x03 => 33,
        0x04 => 35,
        0x05 => 34,
        0x06 => 44,
        0x07 => 45,
        0x08 => 46,
        0x09 => 47,
        0x0b => 48,
        0x0c => 16,
        0x0d => 17,
        0x0e => 18,
        0x0f => 19,
        0x10 => 21,
        0x11 => 20,
        0x12 => 2,
        0x13 => 3,
        0x14 => 4,
        0x15 => 5,
        0x16 => 7,
        0x17 => 6,
        0x18 => 13,
        0x19 => 10,
        0x1a => 8,
        0x1b => 12,
        0x1c => 9,
        0x1d => 11,
        0x1e => 27,
        0x1f => 24,
        0x20 => 22,
        0x21 => 26,
        0x22 => 23,
        0x23 => 25,
        0x24 => 28,
        0x25 => 38,
        0x26 => 36,
        0x27 => 40,
        0x28 => 37,
        0x29 => 39,
        0x2a => 43,
        0x2b => 51,
        0x2c => 53,
        0x2d => 49,
        0x2e => 50,
        0x2f => 52,
        0x30 => 15,
        0x31 => 57,
        0x32 => 41,
        0x33 => 14,
        0x35 => 1,
        0x37 => 125,
        0x38 => 42,
        0x39 => 58,
        0x3a => 56,
        0x3b => 29,
        0x3c => 54,
        0x3d => 100,
        0x3e => 97,
        0x7a => 59,
        0x78 => 60,
        0x63 => 61,
        0x76 => 62,
        0x60 => 63,
        0x61 => 64,
        0x62 => 65,
        0x64 => 66,
        0x65 => 67,
        0x6d => 68,
        0x67 => 87,
        0x6f => 88,
        0x72 => 110,
        0x73 => 102,
        0x74 => 104,
        0x75 => 111,
        0x77 => 107,
        0x79 => 109,
        0x7b => 105,
        0x7c => 106,
        0x7d => 108,
        0x7e => 103,
        _ => return Err(PortalInputError::InvalidEvent),
    })
}

#[cfg(test)]
mod tests;
