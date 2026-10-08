//! HEVC Annex-B access units -> Media Foundation D3D11 decoder surfaces.
//! Runs only on a caller-owned MTA worker, never on the GUI/network executor.
//! A CPU output, unsupported color/format, or unavailable D3D decoder is an error;
//! no hidden software fallback or system codec installation is performed.
use anyhow::{Context, Result, bail, ensure};
use std::{marker::PhantomData, mem::ManuallyDrop, rc::Rc, sync::Arc, time::Instant};
use windows::{
    Win32::{
        Foundation::{E_NOTIMPL, HMODULE},
        Graphics::{
            Direct3D::*,
            Direct3D10::ID3D10Multithread,
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIDevice},
        },
        Media::MediaFoundation::*,
        System::Com::*,
    },
    core::{GUID, Interface},
};

const MAX_ACCESS_UNIT: usize = 16 * 1024 * 1024;
const MAX_OUTPUTS_PER_CALL: usize = 64;
struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("native decoder requires its own COM MTA thread")?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct MfLifetime;
impl MfLifetime {
    fn start() -> Result<Arc<Self>> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        Ok(Arc::new(Self))
    }
}
impl Drop for MfLifetime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}
struct Activations {
    ptr: *mut Option<IMFActivate>,
    len: u32,
}
impl Drop for Activations {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                for i in 0..self.len as usize {
                    std::ptr::drop_in_place(self.ptr.add(i));
                }
                CoTaskMemFree(Some(self.ptr.cast()));
            }
        }
    }
}
struct OutputBuffer(MFT_OUTPUT_DATA_BUFFER);
impl OutputBuffer {
    fn new(stream: u32) -> Self {
        Self(MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: stream,
            pSample: ManuallyDrop::new(None),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        })
    }
}
impl Drop for OutputBuffer {
    fn drop(&mut self) {
        drop(self.0.pSample.take());
        drop(self.0.pEvents.take());
    }
}

/// The actual decoder output, with its MF sample retained for resource lifetime.
/// Holding only the texture is insufficient: the decoder may recycle its array slice.
pub struct D3dDecodedFrame {
    sample: IMFSample,
    texture: ID3D11Texture2D,
    pub subresource: u32,
    pub coded: [u32; 2],
    pub visible: [u32; 4],
    pub sample_aspect: [u32; 2],
    pub format: DXGI_FORMAT,
    pub pts_100ns: i64,
    pub decoded_at: Instant,
    /// Media-type matrix/range/transfer values; None means not signalled by MFT.
    pub matrix: Option<u32>,
    pub nominal_range: Option<u32>,
    pub transfer: Option<u32>,
    pub source_signal: Option<crate::hevc_sequence::VideoSignal>,
    pub chroma_location: Option<u8>,
    _mf: Arc<MfLifetime>,
}
impl D3dDecodedFrame {
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }
    pub fn sample(&self) -> &IMFSample {
        &self.sample
    }
    /// Retain the exact decoder sample through an agile COM reference. This
    /// does not declare IMFSample or the decoder itself Send/Sync, and does not
    /// copy image bytes. Retirement can safely occur after the MTA worker exits.
    pub fn retention_lease(&self) -> Result<Arc<dyn std::any::Any + Send + Sync>> {
        struct Lease {
            // Retention uses only IUnknown's identity/lifetime, not sample methods.
            // Some in-process MFTs have no IMFSample proxy registered. Marshaling
            // IUnknown avoids requiring that optional interface proxy; it still
            // pins the identical sample, and is not an unsafe Send wrapper.
            _sample: windows::core::AgileReference<windows::core::IUnknown>,
            _mf: Arc<MfLifetime>,
        }
        Ok(Arc::new(Lease {
            _sample: windows::core::AgileReference::new(
                &self.sample.cast::<windows::core::IUnknown>()?,
            )?,
            _mf: self._mf.clone(),
        }))
    }
}

/// Per-worker decoder. The marker prohibits moving its COM apartment to another thread.
pub struct WindowsHevcDecoder {
    transform: IMFTransform,
    _activation: IMFActivate,
    manager: IMFDXGIDeviceManager,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    input_id: u32,
    output_id: u32,
    output_type: Option<IMFMediaType>,
    _mf: Arc<MfLifetime>,
    _apartment: Apartment,
    _not_send: PhantomData<Rc<()>>,
    pub decoder_name: String,
    pub adapter_name: String,
    pub input_units: u64,
    pub output_frames: u64,
    pub stream_changes: u64,
    sequence: crate::hevc_sequence::SequenceInfo,
    streaming_started: bool,
    sample_duration_100ns: i64,
}
impl WindowsHevcDecoder {
    pub fn new(sequence: crate::hevc_sequence::SequenceInfo, frame_rate: [u32; 2]) -> Result<Self> {
        ensure!(
            sequence.chroma_format == 1
                && matches!(sequence.bit_depth, 8 | 10)
                && frame_rate[0] > 0
                && frame_rate[1] > 0
                && u64::from(frame_rate[0]) <= 240 * u64::from(frame_rate[1]),
            "invalid native decoder configuration"
        );
        ensure!(
            sequence.coded.iter().all(|v| *v >= 48 && *v <= 8192)
                && sequence.visible[2] > 0
                && sequence.visible[3] > 0
                && sequence.visible[0]
                    .checked_add(sequence.visible[2])
                    .is_some_and(|v| v <= sequence.coded[0])
                && sequence.visible[1]
                    .checked_add(sequence.visible[3])
                    .is_some_and(|v| v <= sequence.coded[1]),
            "invalid decoder source geometry"
        );
        trace("COM start");
        let apartment = Apartment::new()?;
        let mf = MfLifetime::start()?;
        trace("MF started");
        let (mut device, mut context) = (None, None);
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.context("D3D11 returned no decoder device")?;
        let context = context.context("D3D11 returned no immediate context")?;
        let mt: ID3D10Multithread = device
            .cast()
            .or_else(|_| context.cast())
            .context("decoder device lacks multithread protection")?;
        unsafe {
            mt.SetMultithreadProtected(true);
        }
        let dxgi: IDXGIDevice = device.cast()?;
        let adapter = unsafe { dxgi.GetAdapter()?.GetDesc()? };
        let adapter_name = String::from_utf16_lossy(
            &adapter.Description[..adapter
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(adapter.Description.len())],
        );
        let mut reset_cookie = 0u32;
        let mut manager = None;
        unsafe {
            MFCreateDXGIDeviceManager(&mut reset_cookie, &mut manager)?;
        }
        let manager = manager.context("DXGI device manager missing")?;
        unsafe {
            manager.ResetDevice(&device, reset_cookie)?;
        }
        trace("D3D manager ready");
        let input_filter = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_HEVC,
        };
        let mut list = Activations {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input_filter),
                None,
                &mut list.ptr,
                &mut list.len,
            )?;
        }
        ensure!(
            list.len > 0 && list.len <= 128 && !list.ptr.is_null(),
            "No synchronous HEVC decoder installed; existing Windows HEVC component is required"
        );
        trace(&format!("decoder candidates {}", list.len));
        let mut selected = None;
        let mut failures = Vec::new();
        for index in 0..list.len as usize {
            let Some(activation) = unsafe { list.ptr.add(index).as_ref() }.and_then(|v| v.as_ref())
            else {
                continue;
            };
            let name = attribute_string(activation, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "HEVC transform".into());
            let attempt = (|| -> Result<_> {
                trace(&format!("activating {name}"));
                let transform: IMFTransform = unsafe { activation.ActivateObject()? };
                trace("transform activated");
                let attrs = unsafe { transform.GetAttributes()? };
                trace("attributes obtained");
                ensure!(
                    unsafe { attrs.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) == 1,
                    "MFT is not D3D11-aware"
                );
                ensure!(
                    unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) == 0,
                    "asynchronous MFT needs a different event adapter"
                );
                unsafe {
                    attrs.SetUINT32(&MF_LOW_LATENCY, 1)?;
                    trace("before set D3D manager");
                    transform.ProcessMessage(
                        MFT_MESSAGE_SET_D3D_MANAGER,
                        Interface::as_raw(&manager) as usize,
                    )?;
                }
                trace("manager set");
                let (mut input_ids, mut output_ids) = ([0u32], [0u32]);
                if let Err(error) =
                    unsafe { transform.GetStreamIDs(&mut input_ids, &mut output_ids) }
                {
                    ensure!(error.code() == E_NOTIMPL, "GetStreamIDs failed: {error}");
                }
                let input = unsafe { MFCreateMediaType()? };
                unsafe {
                    input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
                    // The HEVC MFT selects its native output family from the input
                    // profile before the first packet. Main10 must not negotiate
                    // an 8-bit output and silently truncate its decoder surfaces.
                    input.SetUINT32(
                        &MF_MT_MPEG2_PROFILE,
                        if sequence.bit_depth == 10 {
                            eAVEncH265VProfile_Main_420_10.0 as u32
                        } else {
                            eAVEncH265VProfile_Main_420_8.0 as u32
                        },
                    )?;
                    input
                        .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    input.SetUINT64(
                        &MF_MT_FRAME_SIZE,
                        (u64::from(sequence.visible[2]) << 32) | u64::from(sequence.visible[3]),
                    )?;
                    input.SetUINT64(
                        &MF_MT_FRAME_RATE,
                        (u64::from(frame_rate[0]) << 32) | u64::from(frame_rate[1]),
                    )?;
                    if let Some(ratio) = sequence.pixel_aspect {
                        input.SetUINT64(
                            &MF_MT_PIXEL_ASPECT_RATIO,
                            (u64::from(ratio[0]) << 32) | u64::from(ratio[1]),
                        )?;
                    }
                    if let Some(signal) = sequence.signal {
                        input.SetUINT32(
                            &MF_MT_VIDEO_NOMINAL_RANGE,
                            if signal.full_range { 1 } else { 2 },
                        )?;
                        if let Some(matrix) = signal.matrix.and_then(|m| match m {
                            1 => Some(1),
                            5 | 6 => Some(2),
                            _ => None,
                        }) {
                            input.SetUINT32(&MF_MT_YUV_MATRIX, matrix)?;
                        }
                    }
                    trace("before input type");
                    transform.SetInputType(input_ids[0], &input, 0)?;
                    trace("input type set");
                }
                if let Ok(attrs) = unsafe { transform.GetOutputStreamAttributes(output_ids[0]) } {
                    unsafe {
                        let _ = attrs.SetUINT32(&MF_SA_MINIMUM_OUTPUT_SAMPLE_COUNT_PROGRESSIVE, 4);
                    }
                }
                Ok((transform, input_ids[0], output_ids[0]))
            })();
            match attempt {
                Ok((transform, input_id, output_id)) => {
                    selected = Some((activation.clone(), transform, name, input_id, output_id));
                    break;
                }
                Err(error) => {
                    failures.push(format!("{name}: {error}"));
                    unsafe {
                        let _ = activation.ShutdownObject();
                    }
                }
            }
        }
        let (activation, transform, decoder_name, input_id, output_id) =
            selected.ok_or_else(|| {
                anyhow::anyhow!("No usable D3D11 HEVC decoder: {}", failures.join("; "))
            })?;
        let mut this = Self {
            transform,
            _activation: activation,
            manager,
            device,
            context,
            input_id,
            output_id,
            output_type: None,
            _mf: mf,
            _apartment: apartment,
            _not_send: PhantomData,
            decoder_name,
            adapter_name,
            input_units: 0,
            output_frames: 0,
            stream_changes: 0,
            sequence,
            streaming_started: false,
            sample_duration_100ns: (10_000_000u64 * u64::from(frame_rate[1])
                / u64::from(frame_rate[0])) as i64,
        };
        // Some MFTs do not know their output extent until the first SPS is parsed.
        trace("before initial output negotiation");
        this.negotiate_output()
            .context("native output must be configured before streaming begins")?;
        trace("after initial output negotiation");
        unsafe {
            this.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            this.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        this.streaming_started = true;
        trace("stream begun");
        Ok(this)
    }
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }
    pub fn device_context(&self) -> &ID3D11DeviceContext {
        &self.context
    }
    fn negotiate_output(&mut self) -> Result<()> {
        let mut candidates = Vec::new();
        for index in 0..32 {
            match unsafe { self.transform.GetOutputAvailableType(self.output_id, index) } {
                Ok(media) => {
                    let subtype = unsafe { media.GetGUID(&MF_MT_SUBTYPE)? };
                    let desired = if self.sequence.bit_depth == 10 {
                        MFVideoFormat_P010
                    } else {
                        MFVideoFormat_NV12
                    };
                    if subtype == desired {
                        candidates.push(media);
                    }
                }
                Err(error) if error.code() == MF_E_NO_MORE_TYPES => break,
                Err(error) => return Err(error.into()),
            }
        }
        for media in candidates {
            if unsafe { self.transform.SetOutputType(self.output_id, &media, 0) }.is_ok() {
                let info = unsafe { self.transform.GetOutputStreamInfo(self.output_id)? };
                ensure!(
                    info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0,
                    "decoder wants application CPU output buffers; native path refused"
                );
                self.output_type = Some(media);
                return Ok(());
            }
        }
        bail!("MFT did not provide a usable native NV12/P010 output type")
    }
    /// One complete compressed access unit, not one fragmented transport datagram.
    /// The input copy is compressed data only, never a decoded image copy.
    pub fn push(
        &mut self,
        bytes: &[u8],
        pts_100ns: i64,
        keyframe: bool,
    ) -> Result<Vec<Arc<D3dDecodedFrame>>> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_ACCESS_UNIT && pts_100ns >= 0,
            "invalid HEVC access unit"
        );
        if let Some(sequence) =
            crate::hevc_sequence::sequence_info(bytes).map_err(anyhow::Error::msg)?
        {
            ensure!(
                sequence == self.sequence,
                "native sequence changed; recreate decoder before accepting this keyframe"
            );
        }
        let sample = unsafe { MFCreateSample()? };
        let buffer = unsafe { MFCreateMemoryBuffer(bytes.len() as u32)? };
        unsafe {
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(bytes.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_100ns)?;
            sample.SetSampleDuration(self.sample_duration_100ns)?;
            sample.SetUINT32(&MFSampleExtension_CleanPoint, u32::from(keyframe))?;
            if self.input_units == 0 {
                sample.SetUINT32(&MFSampleExtension_Discontinuity, 1)?;
            }
        }
        let mut output = Vec::new();
        match unsafe { self.transform.ProcessInput(self.input_id, &sample, 0) } {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                output.extend(self.poll()?);
                unsafe {
                    self.transform
                        .ProcessInput(self.input_id, &sample, 0)
                        .context("MFT still not accepting after output drained")?;
                }
            }
            Err(error) => return Err(anyhow::anyhow!("HEVC input rejected: {error}")),
        }
        self.input_units += 1;
        output.extend(self.poll()?);
        Ok(output)
    }
    pub fn poll(&mut self) -> Result<Vec<Arc<D3dDecodedFrame>>> {
        let mut frames = Vec::new();
        let mut changes = 0;
        for _ in 0..MAX_OUTPUTS_PER_CALL {
            let mut out = OutputBuffer::new(self.output_id);
            let mut status = 0;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, std::slice::from_mut(&mut out.0), &mut status)
            };
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(frames),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    changes += 1;
                    ensure!(
                        changes <= 4,
                        "MFT repeated a format change without progress"
                    );
                    self.negotiate_output()?;
                    self.stream_changes += 1;
                    continue;
                }
                Err(error) => return Err(anyhow::anyhow!("HEVC output failed: {error}")),
                Ok(()) => {}
            }
            let Some(sample) = out.0.pSample.take() else {
                ensure!(
                    out.0.dwStatus == MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE.0 as u32,
                    "MFT output succeeded without a sample"
                );
                return Ok(frames);
            };
            let media = unsafe { self.transform.GetOutputCurrentType(self.output_id)? };
            let buffer = unsafe { sample.GetBufferByIndex(0)? };
            let surface: IMFDXGIBuffer = buffer
                .cast()
                .context("decoder output is CPU-backed; native path refused")?;
            let mut raw = std::ptr::null_mut();
            unsafe {
                surface.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
            }
            ensure!(!raw.is_null(), "MFT returned no native resource");
            let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
            let subresource = unsafe { surface.GetSubresourceIndex()? };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe {
                texture.GetDesc(&mut desc);
            }
            ensure!(
                desc.Usage == D3D11_USAGE_DEFAULT
                    && desc.CPUAccessFlags == 0
                    && desc.BindFlags & D3D11_BIND_DECODER.0 as u32 != 0,
                "MFT did not return a GPU decoder surface"
            );
            ensure!(
                desc.Width > 0
                    && desc.Height > 0
                    && desc.Width <= 8192
                    && desc.Height <= 8192
                    && desc.MipLevels == 1
                    && subresource < desc.ArraySize,
                "invalid decoder allocation or subresource"
            );
            ensure!(
                desc.Format
                    == if self.sequence.bit_depth == 10 {
                        DXGI_FORMAT_P010
                    } else {
                        DXGI_FORMAT_NV12
                    },
                "decoder surface does not preserve the source bit depth: {:?}",
                desc.Format
            );
            let packed = unsafe { media.GetUINT64(&MF_MT_FRAME_SIZE)? };
            let (width, height) = ((packed >> 32) as u32, packed as u32);
            ensure!(
                width > 0 && height > 0 && width <= desc.Width && height <= desc.Height,
                "decoder visible extent exceeds allocation"
            );
            let visible = aperture(&media)?.unwrap_or([0, 0, width, height]);
            ensure!(
                visible[0]
                    .checked_add(visible[2])
                    .is_some_and(|x| x <= desc.Width)
                    && visible[1]
                        .checked_add(visible[3])
                        .is_some_and(|y| y <= desc.Height),
                "decoder aperture exceeds native allocation"
            );
            let ratio =
                unsafe { media.GetUINT64(&MF_MT_PIXEL_ASPECT_RATIO) }.unwrap_or((1u64 << 32) | 1);
            let pts = unsafe { sample.GetSampleTime()? };
            let frame = D3dDecodedFrame {
                sample,
                texture,
                subresource,
                coded: [desc.Width, desc.Height],
                visible,
                sample_aspect: [(ratio >> 32) as u32, ratio as u32],
                format: desc.Format,
                pts_100ns: pts,
                decoded_at: Instant::now(),
                matrix: unsafe { media.GetUINT32(&MF_MT_YUV_MATRIX) }.ok(),
                nominal_range: unsafe { media.GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE) }.ok(),
                transfer: unsafe { media.GetUINT32(&MF_MT_TRANSFER_FUNCTION) }.ok(),
                source_signal: self.sequence.signal,
                chroma_location: self.sequence.chroma_location,
                _mf: self._mf.clone(),
            };
            frames.push(Arc::new(frame));
            self.output_frames += 1;
        }
        bail!("MFT exceeded bounded output work; draining caller must yield")
    }
    /// Drain only at stream end, not between ordinary input packets.
    pub fn finish(&mut self) -> Result<Vec<Arc<D3dDecodedFrame>>> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.poll()
    }
    pub fn flush(&mut self) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        self.input_units = 0;
        Ok(())
    }
}
impl Drop for WindowsHevcDecoder {
    fn drop(&mut self) {
        if !self.streaming_started {
            return;
        }
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            self.context.Flush();
        }
    }
}
fn attribute_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let len = unsafe { attrs.GetStringLength(key) }.ok()?;
    if len > 512 {
        return None;
    }
    let mut data = vec![0u16; len as usize + 1];
    unsafe { attrs.GetString(key, &mut data, None) }.ok()?;
    Some(String::from_utf16_lossy(&data[..len as usize]))
}
fn aperture(media: &IMFMediaType) -> Result<Option<[u32; 4]>> {
    for key in [&MF_MT_MINIMUM_DISPLAY_APERTURE, &MF_MT_GEOMETRIC_APERTURE] {
        let Ok(size) = (unsafe { media.GetBlobSize(key) }) else {
            continue;
        };
        ensure!(size == 16, "unsupported MF aperture size");
        let mut data = [0u8; 16];
        unsafe {
            media.GetBlob(key, &mut data, None)?;
        }
        ensure!(
            u16::from_le_bytes(data[0..2].try_into().unwrap()) == 0
                && u16::from_le_bytes(data[4..6].try_into().unwrap()) == 0,
            "fractional source aperture not supported by integer native crop"
        );
        let x = i16::from_le_bytes(data[2..4].try_into().unwrap());
        let y = i16::from_le_bytes(data[6..8].try_into().unwrap());
        let w = i32::from_le_bytes(data[8..12].try_into().unwrap());
        let h = i32::from_le_bytes(data[12..16].try_into().unwrap());
        ensure!(
            x >= 0 && y >= 0 && w > 0 && h > 0,
            "invalid source aperture"
        );
        return Ok(Some([x as u32, y as u32, w as u32, h as u32]));
    }
    Ok(None)
}

fn trace(message: &str) {
    if std::env::var_os("RP_NATIVE_MF_TRACE").is_some() {
        eprintln!("MF_STAGE {message}");
    }
}
