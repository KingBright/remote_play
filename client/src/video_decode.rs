use async_trait::async_trait;
use gpui::*;
use remote_core::{VideoDecoder, VideoFrame, VideoFrameHandleKind};
use std::error::Error;
use std::ffi::c_void;
use tokio::sync::mpsc;

use core_foundation::base::TCFType;
use core_foundation::base::{CFRelease, OSStatus};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use video_toolbox_sys::cv_types::CVImageBufferRef;
use video_toolbox_sys::decompression::{
    VTDecodeInfoFlags, VTDecompressionSessionCreate, VTDecompressionSessionDecodeFrame,
    VTDecompressionSessionInvalidate, VTDecompressionSessionRef,
    VTDecompressionSessionWaitForAsynchronousFrames,
};

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: *mut c_void,
        parameterSetCount: usize,
        parameterSetPointers: *const *const u8,
        parameterSetSizes: *const usize,
        NALUnitHeaderLength: i32,
        extensions: *mut c_void,
        formatDescriptionOut: *mut *mut c_void,
    ) -> OSStatus;

    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: *mut c_void,
        memoryBlock: *mut c_void,
        blockLength: usize,
        blockAllocator: *mut c_void,
        customBlockSource: *mut c_void,
        offsetToData: usize,
        dataLength: usize,
        flags: u32,
        blockBufferOut: *mut *mut c_void,
    ) -> OSStatus;

    fn CMSampleBufferCreateReady(
        allocator: *mut c_void,
        dataBuffer: *mut c_void,
        formatDescription: *mut c_void,
        numSamples: usize,
        numSampleTimingEntries: usize,
        sampleTimingArray: *const c_void,
        numSampleSizeEntries: usize,
        sampleSizeArray: *const usize,
        sampleBufferOut: *mut *mut c_void,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferGetIOSurface(pixelBuffer: *mut c_void) -> *mut c_void; // IOSurfaceRef
    fn CVPixelBufferGetWidth(pixelBuffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(pixelBuffer: *mut c_void) -> usize;
    fn CVPixelBufferRetain(pixelBuffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(pixelBuffer: *mut c_void);
}

pub struct MacDecodedVideoFrame {
    pub cv_pixel_buffer: *mut c_void,
    pub _io_surface: *mut c_void,
    pub timestamp: u32,
    pub recv_time: u32,
    pub decode_cost_ms: f32,
    pub timing: protocol::FrameTimingCheckpoints,
    pub decoded_at: std::time::Instant,
    width: u32,
    height: u32,
}

unsafe impl Send for MacDecodedVideoFrame {}
unsafe impl Sync for MacDecodedVideoFrame {}

impl Drop for MacDecodedVideoFrame {
    fn drop(&mut self) {
        unsafe {
            if !self.cv_pixel_buffer.is_null() {
                CVPixelBufferRelease(self.cv_pixel_buffer);
            }
        }
    }
}

impl VideoFrame for MacDecodedVideoFrame {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn handle_kind(&self) -> VideoFrameHandleKind {
        VideoFrameHandleKind::MacosCvPixelBuffer
    }
}

pub fn decoded_video_frame_surface(frame: &MacDecodedVideoFrame) -> AnyElement {
    decoded_video_frame_surface_with_fit(frame, gpui::ObjectFit::Contain)
}

pub fn decoded_video_frame_surface_with_fit(
    frame: &MacDecodedVideoFrame,
    object_fit: gpui::ObjectFit,
) -> AnyElement {
    unsafe {
        core_foundation::base::CFRetain(frame.cv_pixel_buffer as *const c_void);
        let cv_pixel_buffer = core_video::pixel_buffer::CVPixelBuffer::wrap_under_create_rule(
            frame.cv_pixel_buffer as _,
        );
        gpui::surface(cv_pixel_buffer)
            .object_fit(object_fit)
            .w_full()
            .h_full()
            .into_any_element()
    }
}

pub struct MacVideoDecoder {
    session: Option<VTDecompressionSessionRef>,
    format_desc: *mut c_void,
    rx: mpsc::Receiver<Option<MacDecodedVideoFrame>>,
    _tx_box: Box<mpsc::Sender<Option<MacDecodedVideoFrame>>>,
    last_vps: Vec<u8>,
    last_sps: Vec<u8>,
    last_pps: Vec<u8>,
}

extern "C" fn decompression_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTDecodeInfoFlags,
    image_buffer: CVImageBufferRef,
    _presentation_time_stamp: core_media_sys::CMTime,
    _presentation_duration: core_media_sys::CMTime,
) {
    let tx = decompression_output_ref_con as *mut mpsc::Sender<Option<MacDecodedVideoFrame>>;

    if status != 0 || image_buffer.is_null() {
        if status != 0 {
            eprintln!("Decoder callback reported error. Status code: {}", status);
        }
        unsafe {
            let _ = (*tx).try_send(None);
        }
        return;
    }

    // A frame dropped
    if (info_flags & 2) != 0 {
        unsafe {
            let _ = (*tx).try_send(None);
        }
        return;
    }

    unsafe {
        let pixel_buffer = image_buffer as *mut c_void;
        let width = CVPixelBufferGetWidth(pixel_buffer) as u32;
        let height = CVPixelBufferGetHeight(pixel_buffer) as u32;
        let io_surface = CVPixelBufferGetIOSurface(pixel_buffer);

        if io_surface.is_null() {
            let _ = (*tx).try_send(None);
            return;
        }

        CVPixelBufferRetain(pixel_buffer); // We need to retain it since we hold it

        let frame = MacDecodedVideoFrame {
            cv_pixel_buffer: pixel_buffer,
            _io_surface: io_surface,
            timestamp: 0, // Will be overridden by main loop
            recv_time: 0, // Will be overridden
            decode_cost_ms: 0.0,
            timing: protocol::FrameTimingCheckpoints::default(),
            decoded_at: std::time::Instant::now(),
            width,
            height,
        };

        let _ = (*tx).try_send(Some(frame));
    }
}

impl MacVideoDecoder {
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let (tx, rx) = mpsc::channel(30);
        let tx_box = Box::new(tx);

        Ok(Self {
            session: None,
            format_desc: std::ptr::null_mut(),
            rx,
            _tx_box: tx_box,
            last_vps: Vec::new(),
            last_sps: Vec::new(),
            last_pps: Vec::new(),
        })
    }

    fn update_format_desc(
        &mut self,
        vps: &[u8],
        sps: &[u8],
        pps: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        unsafe {
            if !self.format_desc.is_null() {
                CFRelease(self.format_desc as _);
            }

            let pointers = [vps.as_ptr(), sps.as_ptr(), pps.as_ptr()];
            let sizes = [vps.len(), sps.len(), pps.len()];

            let mut new_format_desc: *mut c_void = std::ptr::null_mut();
            let status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                std::ptr::null_mut(),
                3,
                pointers.as_ptr(),
                sizes.as_ptr(),
                4, // NALUnitHeaderLength
                std::ptr::null_mut(),
                &mut new_format_desc,
            );

            if status != 0 {
                return Err(format!("Failed to create format desc: {}", status).into());
            }

            self.format_desc = new_format_desc;

            // Recreate session
            if let Some(session) = self.session.take() {
                VTDecompressionSessionInvalidate(session);
                CFRelease(session as _);
            }

            let mut session: VTDecompressionSessionRef = std::ptr::null_mut();

            // Set up Output Callback Record
            let callback_record =
                video_toolbox_sys::decompression::VTDecompressionOutputCallbackRecord {
                    decompressionOutputCallback: decompression_callback,
                    decompressionOutputRefCon: self._tx_box.as_mut() as *mut _ as *mut c_void,
                };

            // Request NV12 Full Range output for GPUI Surface, backed by IOSurface
            let pixel_format = CFNumber::from(875704422_i32); // '420f'
            let pf_key = CFString::from_static_string("PixelFormatType");
            let io_key = CFString::from_static_string("IOSurfaceProperties");
            let empty_dict: core_foundation::dictionary::CFDictionary<CFString, CFString> =
                core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[]);

            let dict = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&[
                (pf_key.as_CFType(), pixel_format.as_CFType()),
                (io_key.as_CFType(), empty_dict.as_CFType()),
            ]);

            let status = VTDecompressionSessionCreate(
                std::ptr::null_mut(),
                self.format_desc as _,
                std::ptr::null_mut(),            // decoderSpecification
                dict.as_CFTypeRef() as *const _, // destinationImageBufferAttributes
                &callback_record,
                &mut session,
            );

            if status != 0 {
                return Err(format!("Failed to create decompression session: {}", status).into());
            }

            // Enable RealTime to minimize latency and buffering
            let rt_key = CFString::from_static_string("RealTime");
            let rt_val = core_foundation::boolean::CFBoolean::true_value();
            video_toolbox_sys::session::VTSessionSetProperty(
                session as _,
                rt_key.as_CFTypeRef() as _,
                rt_val.as_CFTypeRef() as _,
            );

            self.session = Some(session);
        }

        Ok(())
    }
}

unsafe impl Send for MacVideoDecoder {}
unsafe impl Sync for MacVideoDecoder {}

#[async_trait]
impl VideoDecoder for MacVideoDecoder {
    type Frame = MacDecodedVideoFrame;

    async fn decode(&mut self, data: &[u8]) -> Result<Self::Frame, Box<dyn Error + Send + Sync>> {
        // Simple Annex B parsing
        let mut vps = None;
        let mut sps = None;
        let mut pps = None;
        let mut vcl_nalus = Vec::new();

        let mut offset = 0;
        while offset < data.len() {
            // Find start code (0x00000001 or 0x000001)
            let mut start_code_len = 0;
            if offset + 4 <= data.len() && data[offset..offset + 4] == [0, 0, 0, 1] {
                start_code_len = 4;
            } else if offset + 3 <= data.len() && data[offset..offset + 3] == [0, 0, 1] {
                start_code_len = 3;
            }

            if start_code_len > 0 {
                offset += start_code_len;
                let mut next_offset = data.len();
                for i in offset..data.len() {
                    if (i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1])
                        || (i + 3 <= data.len() && data[i..i + 3] == [0, 0, 1])
                    {
                        next_offset = i;
                        break;
                    }
                }

                let nalu = &data[offset..next_offset];
                if !nalu.is_empty() {
                    let nalu_type = (nalu[0] >> 1) & 0x3F;
                    match nalu_type {
                        32 => vps = Some(nalu.to_vec()),
                        33 => sps = Some(nalu.to_vec()),
                        34 => pps = Some(nalu.to_vec()),
                        _ => {
                            // Filter ONLY VCL NAL units (1 to 21)
                            if (1..=21).contains(&nalu_type) {
                                // Convert to AVCC (length prefixed)
                                let len = (nalu.len() as u32).to_be_bytes();
                                vcl_nalus.extend_from_slice(&len);
                                vcl_nalus.extend_from_slice(nalu);
                            }
                        }
                    }
                }
                offset = next_offset;
            } else {
                offset += 1;
            }
        }

        if let (Some(v), Some(s), Some(p)) = (vps, sps, pps)
            && (self.session.is_none()
                || v != self.last_vps
                || s != self.last_sps
                || p != self.last_pps)
        {
            self.last_vps = v.clone();
            self.last_sps = s.clone();
            self.last_pps = p.clone();
            self.update_format_desc(&v, &s, &p)?;
        }

        if vcl_nalus.is_empty() || self.session.is_none() {
            // No VCL data or no format desc yet
            return Err("No frame data or session not ready".into());
        }

        unsafe {
            // Create CMBlockBuffer
            let mut block_buffer: *mut c_void = std::ptr::null_mut();
            let status = CMBlockBufferCreateWithMemoryBlock(
                core_foundation::base::kCFAllocatorDefault as *mut _,
                vcl_nalus.as_mut_ptr() as *mut c_void,
                vcl_nalus.len(),
                core_foundation::base::kCFAllocatorNull as *mut _, // MUST use kCFAllocatorNull to prevent double-free
                std::ptr::null_mut(),
                0,
                vcl_nalus.len(),
                0,
                &mut block_buffer,
            );

            if status != 0 {
                return Err(format!("Block buffer creation failed: {}", status).into());
            }

            // Create CMSampleBuffer
            let sample_size = vcl_nalus.len();
            let mut sample_buffer: *mut c_void = std::ptr::null_mut();
            let status = CMSampleBufferCreateReady(
                std::ptr::null_mut(),
                block_buffer,
                self.format_desc,
                1,
                0,
                std::ptr::null(),
                1,
                &sample_size,
                &mut sample_buffer,
            );

            if status != 0 {
                CFRelease(block_buffer as _);
                return Err(format!("Sample buffer creation failed: {}", status).into());
            }

            let session = self.session.unwrap();
            let mut info_flags: VTDecodeInfoFlags = 0;
            let status = VTDecompressionSessionDecodeFrame(
                session,
                sample_buffer as _,
                0,
                std::ptr::null_mut(),
                &mut info_flags,
            );

            CFRelease(sample_buffer as _);
            CFRelease(block_buffer as _);

            if status != 0 {
                return Err(format!("Decode failed: {}", status).into());
            }
        }

        let start_time = std::time::Instant::now();
        // Add a timeout to prevent deadlocks if the callback is never called
        match tokio::time::timeout(std::time::Duration::from_millis(200), self.rx.recv()).await {
            Ok(Some(Some(mut frame))) => {
                frame.decode_cost_ms = start_time.elapsed().as_secs_f32() * 1000.0;
                Ok(frame)
            }
            Ok(Some(None)) => Err("Decoder callback reported error or dropped frame.".into()),
            Ok(None) => Err("Decoder channel closed".into()),
            Err(_) => Err("Decoder callback timeout (frame dropped by VT or buffered).".into()),
        }
    }
}

impl Drop for MacVideoDecoder {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            unsafe {
                VTDecompressionSessionWaitForAsynchronousFrames(session);
                VTDecompressionSessionInvalidate(session);
                CFRelease(session as _);
            }
        }
        if !self.format_desc.is_null() {
            unsafe {
                CFRelease(self.format_desc as _);
            }
        }
    }
}
