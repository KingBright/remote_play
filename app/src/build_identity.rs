//! Immutable bytes selected by the product build script, never a runtime sidecar.
include!(concat!(env!("OUT_DIR"), "/compiled_build_identity.rs"));

pub fn compiled() -> Option<serde_json::Value> {
    let bytes = build_identity_bytes();
    if bytes.is_empty() {
        None
    } else {
        Some(serde_json::from_slice(bytes).expect("validated compiled build identity"))
    }
}
