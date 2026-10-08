//! Explicit local synthetic-clip decoder check; no network, capture or input.
#[cfg(target_os = "windows")]
#[path = "support/hevc_fixture.rs"]
mod fixture;
#[cfg(target_os = "windows")]
use fixture::access_units;
#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use client::windows_hevc::WindowsHevcDecoder;
    use std::{path::PathBuf, time::Instant};
    let path = PathBuf::from(std::env::var("RP_NATIVE_HEVC_FIXTURE")?);
    let out = PathBuf::from(std::env::var("RP_NATIVE_HEVC_RECEIPT")?);
    if out.exists() {
        return Err("refusing to overwrite test receipt".into());
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("test clip too large".into());
    }
    let units = access_units(&bytes);
    if units.is_empty() {
        return Err("no Annex B access units".into());
    }
    let info = client::hevc_sequence::sequence_info(&bytes)?.ok_or("no sequence header")?;
    let mut decoder = WindowsHevcDecoder::new(info, [30, 1])?;
    println!(
        "NATIVE_HEVC_DECODER {} GPU {} units={}",
        decoder.decoder_name,
        decoder.adapter_name,
        units.len()
    );
    let mut records = Vec::new();
    let began = Instant::now();
    let mut costs = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let start = Instant::now();
        let frames = decoder.push(
            unit,
            i as i64 * 333333,
            remote_core::media_plane::is_hevc_keyframe(unit),
        )?;
        costs.push(start.elapsed().as_micros() as u64);
        for frame in frames {
            records.push(serde_json::json!({"coded":frame.coded,"visible":frame.visible,"subresource":frame.subresource,"pts":frame.pts_100ns,"format":frame.format.0,"matrix":frame.matrix,"range":frame.nominal_range,"transfer":frame.transfer}));
        }
    }
    for frame in decoder.finish()? {
        records.push(serde_json::json!({"coded":frame.coded,"visible":frame.visible,"subresource":frame.subresource,"pts":frame.pts_100ns,"format":frame.format.0}));
    }
    if records.len() != units.len() {
        return Err(format!("decoded {} of {} access units", records.len(), units.len()).into());
    }
    let report = serde_json::json!({"passed":true,"decoder":decoder.decoder_name,"adapter":decoder.adapter_name,"input_access_units":units.len(),"native_output_frames":records.len(),"elapsed_ms":began.elapsed().as_millis(),"submission_wall_us":costs,"frames":records,"cpu_pixel_mapping":false,"screen_capture":false,"network":false,"gui_rendered":false});
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    println!("NATIVE_HEVC_DECODE_PASS {} frames", records.len());
    Ok(())
}
#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("This explicit decoder check requires Windows D3D11/Media Foundation.");
}
