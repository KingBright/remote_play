//! Real hardware encoder regression using generated NV12 buffers, not capture.
use super::*;
use core_foundation::base::CFTypeRef;
use screencapturekit::{
    cm::{CMSampleBuffer, CMTime as SampleTime},
    cv::CVPixelBuffer,
};
use std::time::Duration;
#[path = "../../client/src/hevc_sequence.rs"]
mod hevc_sequence;

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVBufferSetAttachment(buffer: *const c_void, key: CFTypeRef, value: CFTypeRef, mode: u32);
    static kCVImageBufferColorPrimariesKey: CFTypeRef;
    static kCVImageBufferTransferFunctionKey: CFTypeRef;
    static kCVImageBufferYCbCrMatrixKey: CFTypeRef;
    static kCVImageBufferColorPrimaries_ITU_R_709_2: CFTypeRef;
    static kCVImageBufferTransferFunction_ITU_R_709_2: CFTypeRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_709_2: CFTypeRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_601_4: CFTypeRef;
}
fn generated_frame(
    width: u32,
    height: u32,
    index: i64,
    matrix709: bool,
) -> Result<MacVideoFrame, Box<dyn Error + Send + Sync>> {
    let buffer = CVPixelBuffer::create(width as usize, height as usize, 0x34323076)
        .map_err(|e| format!("synthetic CV buffer: {e}"))?;
    {
        let mut guard = buffer
            .lock_read_write()
            .map_err(|e| format!("synthetic CV lock: {e}"))?;
        if guard.plane_count() != 2 {
            return Err("test requires NV12 two-plane buffer".into());
        }
        for plane in 0..2 {
            let size = guard
                .bytes_per_row_of_plane(plane)
                .checked_mul(guard.height_of_plane(plane))
                .ok_or("plane overflow")?;
            let ptr = guard
                .base_address_of_plane_mut(plane)
                .ok_or("test plane missing")?;
            unsafe {
                std::ptr::write_bytes(ptr, if plane == 0 { 96 } else { 128 }, size);
            }
        }
    }
    unsafe {
        for (key, value) in [
            (
                kCVImageBufferColorPrimariesKey,
                kCVImageBufferColorPrimaries_ITU_R_709_2,
            ),
            (
                kCVImageBufferTransferFunctionKey,
                kCVImageBufferTransferFunction_ITU_R_709_2,
            ),
            (
                kCVImageBufferYCbCrMatrixKey,
                if matrix709 {
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2
                } else {
                    kCVImageBufferYCbCrMatrix_ITU_R_601_4
                },
            ),
        ] {
            CVBufferSetAttachment(buffer.as_ptr().cast(), key, value, 1);
        }
    }
    let sample = CMSampleBuffer::create_for_image_buffer(
        &buffer,
        SampleTime::new(index, 30),
        SampleTime::new(1, 30),
    )
    .map_err(|e| format!("synthetic sample: {e}"))?;
    Ok(MacVideoFrame {
        width,
        height,
        sample_buffer: sample,
        capture_time_ms: (index * 33) as u32,
        timing: protocol::FrameTimingCheckpoints::default(),
    })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_source_color_survives_hardware_hevc_and_session_resize()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let mut encoder = MacVideoEncoder::new(640, 360, 30, 2000)?;
    for (index, (w, h, matrix709, expected)) in [
        (640, 360, true, 1),
        (640, 360, false, 6),
        (320, 240, true, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let frame = generated_frame(w, h, index as i64, matrix709)?;
        encoder.submit_frame(frame).await?;
        unsafe {
            let status = VTCompressionSessionCompleteFrames(
                encoder.session.unwrap(),
                core_media_sys::CMTime {
                    value: 0,
                    timescale: 0,
                    flags: 0,
                    epoch: 0,
                },
            );
            if status != 0 {
                return Err(format!("test encoder flush: {status}").into());
            }
        }
        let chunk =
            tokio::time::timeout(Duration::from_secs(5), encoder.pull_encoded_chunk()).await??;
        let sequence =
            hevc_sequence::sequence_info(&chunk.nalu)?.ok_or("hardware output omitted SPS")?;
        let signal = sequence
            .signal
            .ok_or("hardware output omitted color signal")?;
        assert_eq!(
            signal.matrix,
            Some(expected),
            "the source matrix must be encoded, not inferred by receiver"
        );
        assert_eq!(signal.primaries, Some(1));
        assert_eq!(signal.transfer, Some(1));
        assert!(!signal.full_range);
        assert_eq!([sequence.visible[2], sequence.visible[3]], [w, h]);
        println!(
            "SOURCE_COLOR_HEVC_VERIFIED size={w}x{h} matrix={expected} primaries=1 transfer=1 range=limited keyframe={}",
            chunk.is_keyframe
        );
    }
    Ok(())
}
