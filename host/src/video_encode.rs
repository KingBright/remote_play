use crate::capture::MacVideoFrame;
use async_trait::async_trait;
use core_foundation::base::TCFType;
use core_foundation::base::{CFRelease, OSStatus};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_media_sys::CMTime;
use remote_core::{VideoEncoder, VideoFrame};
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

pub struct MacVideoEncoder {
    session: Option<VTCompressionSessionRef>,
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
    _tx_box: Box<mpsc::Sender<Vec<u8>>>,
}

extern "C" fn compression_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: *mut c_void,
) {
    if status != 0 || sample_buffer.is_null() {
        unsafe {
            let tx = output_callback_ref_con as *mut mpsc::Sender<Vec<u8>>;
            let _ = (*tx).try_send(vec![]);
        }
        return;
    }

    unsafe {
        let block_buffer = CMSampleBufferGetDataBuffer(sample_buffer);
        if block_buffer.is_null() {
            let tx = output_callback_ref_con as *mut mpsc::Sender<Vec<u8>>;
            let _ = (*tx).try_send(vec![]);
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
            let tx = output_callback_ref_con as *mut mpsc::Sender<Vec<u8>>;
            let _ = (*tx).try_send(vec![]);
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
            println!("Detected I-Frame in VTCallback! Generating parameter sets...");
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
            let tx = output_callback_ref_con as *mut mpsc::Sender<Vec<u8>>;
            let _ = (*tx).try_send(out_buffer);
        }
    }
}

impl MacVideoEncoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate_kbps: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (tx, rx) = mpsc::channel(60);
        let tx_box = Box::new(tx.clone());
        let ref_con = Box::into_raw(tx_box.clone()) as *mut c_void;

        let mut session: VTCompressionSessionRef = std::ptr::null_mut();

        let hevc_codec_type = 0x68766331; // 'hvc1'
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null_mut(),
                width as i32,
                height as i32,
                hevc_codec_type,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
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
            VTSessionSetProperty(
                session,
                profile_key.as_concrete_TypeRef() as _,
                profile_value.as_concrete_TypeRef() as _,
            );

            let realtime_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_RealTime as _);
            VTSessionSetProperty(
                session,
                realtime_key.as_concrete_TypeRef() as _,
                CFBoolean::true_value().as_concrete_TypeRef() as _,
            );

            let reordering_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AllowFrameReordering as _);
            VTSessionSetProperty(
                session,
                reordering_key.as_concrete_TypeRef() as _,
                CFBoolean::false_value().as_concrete_TypeRef() as _,
            );

            let final_bitrate = (bitrate_kbps as i64) * 1000;

            let bitrate_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_AverageBitRate as _);
            let bitrate_value = CFNumber::from(final_bitrate);
            VTSessionSetProperty(
                session,
                bitrate_key.as_concrete_TypeRef() as _,
                bitrate_value.as_concrete_TypeRef() as _,
            );

            let keyframe_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_MaxKeyFrameInterval as _);
            let keyframe_value = CFNumber::from(fps as i32);
            VTSessionSetProperty(
                session,
                keyframe_key.as_concrete_TypeRef() as _,
                keyframe_value.as_concrete_TypeRef() as _,
            );

            let keyframe_dur_key = CFString::wrap_under_get_rule(
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration as _,
            );
            let keyframe_dur_value = CFNumber::from(1.0f64);
            VTSessionSetProperty(
                session,
                keyframe_dur_key.as_concrete_TypeRef() as _,
                keyframe_dur_value.as_concrete_TypeRef() as _,
            );

            let fps_key =
                CFString::wrap_under_get_rule(kVTCompressionPropertyKey_ExpectedFrameRate as _);
            let fps_value = CFNumber::from(fps as f64);
            VTSessionSetProperty(
                session,
                fps_key.as_concrete_TypeRef() as _,
                fps_value.as_concrete_TypeRef() as _,
            );

            let hw_key = CFString::wrap_under_get_rule(
                kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder as _,
            );
            VTSessionSetProperty(
                session,
                hw_key.as_concrete_TypeRef() as _,
                CFBoolean::true_value().as_concrete_TypeRef() as _,
            );
        }

        Ok(Self {
            session: Some(session),
            tx,
            rx,
            _tx_box: tx_box,
        })
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
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut info_flags_out,
            );

            if status != 0 {
                return Err(format!("Encode failed: {}", status).into());
            }
        }

        Ok(())
    }

    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        if let Some(encoded_data) = self.rx.recv().await {
            Ok(encoded_data)
        } else {
            Err("Encoder channel closed".into())
        }
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
                CFRelease(session as *const c_void);
            }
        }
    }
}
