//! Exercise the actual desktop pairing controller without linking the GPUI/media stack.
//! The controller uses only the same remote_core, Tokio and serde dependencies.
#[allow(dead_code)]
#[path = "../../client/src/mesh_pairing.rs"]
mod mesh_pairing;
