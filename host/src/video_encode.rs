use crate::capture::MacVideoFrame;
use async_trait::async_trait;
use core_foundation::base::TCFType;
use core_foundation::base::{CFRelease, OSStatus};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_media_sys::CMTime;
use remote_core::VideoEncoder;
use std::error::Error;
use std::ffi::c_void;
use tokio::sync::mpsc;
use video_toolbox_sys::compression::{
    VTCompressionSessionCompleteFrames, VTCompressionSessionCreate,
    VTCompressionSessionEncodeFrame, VTCompressionSessionInvalidate, VTCompressionSessionRef,
};
use video_toolbox_sys::compression::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder,
};
use video_toolbox_sys::session::VTSessionSetProperty;

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
        videoDesc: *mut c_void,
        parameterSetIndex: usize,
        parameterSetPointerOut: *mut *const u8,
        parameterSetSizeOut: *mut usize,
        parameterSetCountOut: *mut usize,
        NALUnitHeaderLengthOut: *mut std::os::raw::c_int,
    ) -> OSStatus;

    fn CMSampleBufferGetDataBuffer(sbuf: *mut c_void) -> *mut c_void;

    fn CMBlockBufferGetDataPointer(
        theBuffer: *mut c_void,
        offset: usize,
        lengthAtOffsetOut: *mut usize,
        totalLengthOut: *mut usize,
        dataPointerOut: *mut *mut u8,
    ) -> OSStatus;

    fn CMSampleBufferGetFormatDescription(sbuf: *mut c_void) -> *mut c_void;

    fn CMSampleBufferGetSampleAttachmentsArray(
        sbuf: *mut c_void,
        createIfNecessary: bool,
    ) -> *mut c_void; // CFArrayRef
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(theArray: *mut c_void) -> isize;
    fn CFArrayGetValueAtIndex(theArray: *mut c_void, idx: isize) -> *mut c_void;
    fn CFDictionaryContainsKey(theDict: *mut c_void, key: *const c_void) -> bool;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    static kCMSampleAttachmentKey_NotSync: *const c_void;
}

#[derive(Clone, Debug)]
pub struct EncodedChunk {
    pub nalu: Vec<u8>,
    pub capture_time_ms: u32,
    pub encode_cost_ms: f32,
    pub is_keyframe: bool,
}

struct FrameContext {
    capture_time_ms: u32,
    submit_instant: std::time::Instant,
}

pub struct MacVideoEncoder {
    session: Option<VTCompressionSessionRef>,
    _tx: mpsc::Sender<EncodedChunk>,
    rx: mpsc::Receiver<EncodedChunk>,
    _tx_box: Box<mpsc::Sender<EncodedChunk>>,
    force_keyframe: bool,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
}

extern "C" fn compression_callback(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: *mut c_void,
) {
    let (capture_time_ms, encode_cost_ms) = if !source_frame_ref_con.is_null() {
        let ctx = unsafe { Box::from_raw(source_frame_ref_con as *mut FrameContext) };
        let cost = ctx.submit_instant.elapsed().as_secs_f32() * 1000.0;
        (ctx.capture_time_ms, cost)
    } else {
        (
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                & 0xFFFFFFFF) as u32,
            2.0,
        )
    };

    if status != 0 || sample_buffer.is_null() {
        return;
    }

    unsafe {
        let block_buffer = CMSampleBufferGetDataBuffer(sample_buffer);
        if block_buffer.is_null() {
            return;
        }

        let mut length_at_offset = 0;
        let mut total_length = 0;
        let mut data_ptr: *mut u8 = std::ptr::null_mut();

        if CMBlockBufferGetDataPointer(
            block_buffer,
            0,
            &mut length_at_offset,
            &mut total_length,
            &mut data_ptr,
        ) != 0
        {
            return;
        }

        let mut out_buffer = Vec::with_capacity(total_length + 256);
        let mut offset = 0;
        let mut is_keyframe = true;

        // Use CoreMedia attachments to check for keyframe (NotSync)
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample_buffer, false);
        if !attachments.is_null() && CFArrayGetCount(attachments) > 0 {
            let dict = CFArrayGetValueAtIndex(attachments, 0);
            if CFDictionaryContainsKey(dict, kCMSampleAttachmentKey_NotSync) {
                is_keyframe = false;
            }
        }

        // Extract VPS, SPS, PPS if keyframe
        if is_keyframe {
            let format_desc = CMSampleBufferGetFormatDescription(sample_buffer);
            if !format_desc.is_null() {
                for i in 0..3 {
                    let mut param_ptr: *const u8 = std::ptr::null();
                    let mut param_size = 0;
                    let mut param_count = 0;
                    let mut header_length = 0;
                    if CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                        format_desc,
                        i,
                        &mut param_ptr,
                        &mut param_size,
                        &mut param_count,
                        &mut header_length,
                    ) == 0
                        && !param_ptr.is_null()
                    {
                        let param_slice = std::slice::from_raw_parts(param_ptr, param_size);
                        out_buffer.extend_from_slice(&[0, 0, 0, 1]);
                        out_buffer.extend_from_slice(param_slice);
                    }
                }
            }
        }

        // Convert AVCC to Annex B
        while offset + 4 <= total_length {
            let nalu_length = u32::from_be_bytes([
                *data_ptr.add(offset),
                *data_ptr.add(offset + 1),
                *data_ptr.add(offset + 2),
                *data_ptr.add(offset + 3),
            ]) as usize;

            if offset + 4 + nalu_length > total_length {
                break;
            }

            out_buffer.extend_from_slice(&[0, 0, 0, 1]);
            let nalu_data = std::slice::from_raw_parts(data_ptr.add(offset + 4), nalu_length);
            out_buffer.extend_from_slice(nalu_data);

            offset += 4 + nalu_length;
        }

        if !out_buffer.is_empty() {
            let tx = output_callback_ref_con as *mut mpsc::Sender<EncodedChunk>;
            let _ = (*tx).try_send(EncodedChunk {
                nalu: out_buffer,
                capture_time_ms,
                encode_cost_ms,
                is_keyframe,
            });
        }
    }
}

impl MacVideoEncoder {
    unsafe fn create_session(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        ref_con: *mut c_void,
    ) -> Result<VTCompressionSessionRef, Box<dyn Error + Send + Sync>> {
        let mut session: VTCompressionSessionRef = std::ptr::null_mut();
        let hevc_codec_type = 0x68766331; // 'hvc1'

        let hw_key = CFString::from_static_string("RequireHardwareAcceleratedVideoEncoder");
        let encoder_spec = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[(
            hw_key.as_CFType(),
            CFBoolean::true_value().as_CFType(),
        )]);

        // Zero-copy IOSurface and Metal memory buffer specification
        let io_surface_dict: core_foundation::dictionary::CFDictionary<CFString, CFBoolean> =
            core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[]);
        let io_surface_key = CFString::from_static_string("IOSurfaceProperties");
        let metal_key = CFString::from_static_string("MetalCompatibility");
        let source_attributes = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[
            (io_surface_key.as_CFType(), io_surface_dict.as_CFType()),
            (metal_key.as_CFType(), CFBoolean::true_value().as_CFType()),
        ]);

        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null_mut(),
                width as i32,
                height as i32,
                hevc_codec_type,
                encoder_spec.as_concrete_TypeRef() as _,
                source_attributes.as_concrete_TypeRef() as _,
                std::ptr::null_mut(),
                compression_callback,
                ref_con,
                &mut session,
            )
        };

        if status != 0 {
            return Err(format!("Failed to create VTCompressionSession. Error: {}", status).into());
        }

        unsafe {
            let profile_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ProfileLevel as _);
            let profile_value =
                CFString::wrap_under_get_rule(kVTProfileLevel_HEVC_Main_AutoLevel as _);
            let _ = VTSessionSetProperty(
                session,
                profile_key.as_concrete_TypeRef() as _,
                profile_value.as_concrete_TypeRef() as _,
            );

            let realtime_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_RealTime as _);
            let _ = VTSessionSetProperty(
                session,
                realtime_key.as_concrete_TypeRef() as _,
                CFBoolean::true_value().as_concrete_TypeRef() as _,
            );

            let reordering_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AllowFrameReordering as _);
            let _ = VTSessionSetProperty(
                session,
                reordering_key.as_concrete_TypeRef() as _,
                CFBoolean::false_value().as_concrete_TypeRef() as _,
            );

            // Zero internal frame delay count: force immediate emission of compressed frame
            let max_delay_key = CFString::from_static_string("MaxFrameDelayCount");
            let max_delay_val = CFNumber::from(0_i32);
            let _ = VTSessionSetProperty(
                session,
                max_delay_key.as_concrete_TypeRef() as _,
                max_delay_val.as_concrete_TypeRef() as _,
            );

            let temp_comp_key = CFString::from_static_string("AllowTemporalCompression");
            let _ = VTSessionSetProperty(
                session,
                temp_comp_key.as_concrete_TypeRef() as _,
                CFBoolean::true_value().as_concrete_TypeRef() as _,
            );

            // Prioritize encoding speed over quality for ultra-low latency gaming
            let speed_key = CFString::from_static_string("PrioritizeEncodingSpeedOverQuality");
            let _ = VTSessionSetProperty(
                session,
                speed_key.as_concrete_TypeRef() as _,
                CFBoolean::true_value().as_concrete_TypeRef() as _,
            );

            let power_key = CFString::from_static_string("MaximizePowerEfficiency");
            let _ = VTSessionSetProperty(
                session,
                power_key.as_concrete_TypeRef() as _,
                CFBoolean::false_value().as_concrete_TypeRef() as _,
            );

            let final_bitrate = (bitrate_kbps as i64) * 1000;

            let bitrate_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as _);
            let bitrate_value = CFNumber::from(final_bitrate);
            let _ = VTSessionSetProperty(
                session,
                bitrate_key.as_concrete_TypeRef() as _,
                bitrate_value.as_concrete_TypeRef() as _,
            );

            // Fast single-pass CBR rate limit (eliminates multi-pass RDO lag)
            let bytes_per_sec = (final_bitrate / 8) as i32;
            let one_sec = 1_i32;
            let num_bytes = CFNumber::from(bytes_per_sec);
            let num_sec = CFNumber::from(one_sec);
            let limit_array = core_foundation::array::CFArray::from_CFTypes(&[
                num_bytes.as_CFType(),
                num_sec.as_CFType(),
            ]);
            let data_rate_limits_key = CFString::from_static_string("DataRateLimits");
            let _ = VTSessionSetProperty(
                session,
                data_rate_limits_key.as_concrete_TypeRef() as _,
                limit_array.as_concrete_TypeRef() as _,
            );

            let fps_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ExpectedFrameRate as _);
            let fps_value = CFNumber::from(fps as i32);
            let _ = VTSessionSetProperty(
                session,
                fps_key.as_concrete_TypeRef() as _,
                fps_value.as_concrete_TypeRef() as _,
            );

            // Steady keyframe interval (10s) to eliminate periodic network bursts
            let keyframe_interval_key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_MaxKeyFrameInterval as _,
            );
            let keyframe_interval_value = CFNumber::from((fps * 10) as i32);
            let _ = VTSessionSetProperty(
                session,
                keyframe_interval_key.as_concrete_TypeRef() as _,
                keyframe_interval_value.as_concrete_TypeRef() as _,
            );

            let keyframe_duration_key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration as _,
            );
            let keyframe_duration_value = CFNumber::from(10 as i32);
            let _ = VTSessionSetProperty(
                session,
                keyframe_duration_key.as_concrete_TypeRef() as _,
                keyframe_duration_value.as_concrete_TypeRef() as _,
            );
        }

        Ok(session)
    }

    pub fn new(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (tx, rx) = mpsc::channel(60);
        let tx_box = Box::new(tx.clone());
        let ref_con = Box::into_raw(tx_box.clone()) as *mut c_void;

        let session = unsafe { Self::create_session(width, height, fps, bitrate_kbps, ref_con)? };

        Ok(Self {
            session: Some(session),
            _tx: tx,
            rx,
            _tx_box: tx_box,
            force_keyframe: true,
            width,
            height,
            fps,
            bitrate_kbps,
        })
    }

    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    pub fn update_settings(&mut self, width: u32, height: u32, fps: u32, bitrate_kbps: u32) {
        if self.width == width && self.height == height {
            self.fps = fps;
            self.bitrate_kbps = bitrate_kbps;
            if let Some(session) = self.session {
                unsafe {
                    let final_bitrate = (bitrate_kbps as i64) * 1000;
                    let bitrate_key =
                        CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as _);
                    let bitrate_value = CFNumber::from(final_bitrate);
                    let _ = VTSessionSetProperty(
                        session,
                        bitrate_key.as_concrete_TypeRef() as _,
                        bitrate_value.as_concrete_TypeRef() as _,
                    );

                    let bytes_per_sec = (final_bitrate / 8) as i32;
                    let one_sec = 1_i32;
                    let num_bytes = CFNumber::from(bytes_per_sec);
                    let num_sec = CFNumber::from(one_sec);
                    let limit_array = core_foundation::array::CFArray::from_CFTypes(&[
                        num_bytes.as_CFType(),
                        num_sec.as_CFType(),
                    ]);
                    let data_rate_limits_key = CFString::from_static_string("DataRateLimits");
                    let _ = VTSessionSetProperty(
                        session,
                        data_rate_limits_key.as_concrete_TypeRef() as _,
                        limit_array.as_concrete_TypeRef() as _,
                    );

                    let fps_key =
                        CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ExpectedFrameRate as _);
                    let fps_value = CFNumber::from(fps as i32);
                    let _ = VTSessionSetProperty(
                        session,
                        fps_key.as_concrete_TypeRef() as _,
                        fps_value.as_concrete_TypeRef() as _,
                    );
                }
            }
        } else {
            // Recreate session for new resolution and trigger mandatory IDR KeyFrame
            if let Some(old_session) = self.session.take() {
                unsafe {
                    VTCompressionSessionInvalidate(old_session);
                    CFRelease(old_session as _);
                }
            }
            let ref_con = self._tx_box.as_mut() as *mut _ as *mut c_void;
            if let Ok(new_session) = unsafe { Self::create_session(width, height, fps, bitrate_kbps, ref_con) } {
                self.session = Some(new_session);
                self.width = width;
                self.height = height;
                self.fps = fps;
                self.bitrate_kbps = bitrate_kbps;
                self.force_keyframe = true;
            }
        }
    }

    pub async fn pull_encoded_chunk(&mut self) -> Result<EncodedChunk, Box<dyn Error + Send + Sync>> {
        if let Some(chunk) = self.rx.recv().await {
            Ok(chunk)
        } else {
            Err("Encoder channel closed".into())
        }
    }
}

unsafe impl Send for MacVideoEncoder {}
unsafe impl Sync for MacVideoEncoder {}

#[async_trait]
impl VideoEncoder for MacVideoEncoder {
    type Frame = MacVideoFrame;

    async fn submit_frame(
        &mut self,
        frame: Self::Frame,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let cv_pixel_buffer = match frame.sample_buffer.image_buffer() {
            Some(buf) => buf,
            None => return Ok(()),
        };
        let pts = frame.sample_buffer.presentation_timestamp();
        let duration = frame.sample_buffer.duration();

        let session = self.session.unwrap();

        let ctx = Box::new(FrameContext {
            capture_time_ms: frame.capture_time_ms,
            submit_instant: std::time::Instant::now(),
        });
        let source_frame_ref_con = Box::into_raw(ctx) as *mut c_void;

        let frame_props_dict = if self.force_keyframe {
            self.force_keyframe = false;
            let force_key = CFString::from_static_string("ForceKeyFrame");
            let dict = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[(
                force_key.as_CFType(),
                CFBoolean::true_value().as_CFType(),
            )]);
            Some(dict)
        } else {
            None
        };
        let frame_props = frame_props_dict
            .as_ref()
            .map(|d| d.as_concrete_TypeRef() as _)
            .unwrap_or(std::ptr::null());

        unsafe {
            let pts_sys = core_media_sys::CMTime {
                value: pts.value,
                timescale: pts.timescale,
                flags: pts.flags,
                epoch: pts.epoch,
            };
            let dur_sys = core_media_sys::CMTime {
                value: duration.value,
                timescale: duration.timescale,
                flags: duration.flags,
                epoch: duration.epoch,
            };
            let mut info_flags_out: u32 = 0;

            let status = VTCompressionSessionEncodeFrame(
                session,
                cv_pixel_buffer.as_ptr() as _,
                pts_sys,
                dur_sys,
                frame_props,
                source_frame_ref_con,
                &mut info_flags_out,
            );

            if status != 0 {
                // If encode failed, reclaim ctx to prevent memory leak
                let _ = Box::from_raw(source_frame_ref_con as *mut FrameContext);
                return Err(format!("Encode failed: {}", status).into());
            }
        }

        Ok(())
    }

    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let chunk = self.pull_encoded_chunk().await?;
        Ok(chunk.nalu)
    }
}

impl Drop for MacVideoEncoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            unsafe {
                VTCompressionSessionCompleteFrames(
                    session,
                    CMTime {
                        value: 0,
                        timescale: 0,
                        flags: 0,
                        epoch: 0,
                    },
                );
                VTCompressionSessionInvalidate(session);
                CFRelease(session);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dynamic_encoder_resolution_and_bitrate_switch() {
        let mut encoder = MacVideoEncoder::new(1920, 1080, 60, 20_000).expect("create encoder");
        assert_eq!(encoder.width, 1920);
        assert_eq!(encoder.height, 1080);
        assert_eq!(encoder.fps, 60);
        assert_eq!(encoder.bitrate_kbps, 20_000);

        // Dynamic bitrate and fps switch
        encoder.update_settings(1920, 1080, 30, 5_000);
        assert_eq!(encoder.fps, 30);
        assert_eq!(encoder.bitrate_kbps, 5_000);

        // Dynamic resolution switch (720p)
        encoder.update_settings(1280, 720, 60, 10_000);
        assert_eq!(encoder.width, 1280);
        assert_eq!(encoder.height, 720);
        assert_eq!(encoder.fps, 60);
        assert_eq!(encoder.bitrate_kbps, 10_000);
        assert!(encoder.force_keyframe);
    }
}
