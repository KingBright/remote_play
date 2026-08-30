use crate::clipboard_plane::ClipboardSyncPolicy;
use async_trait::async_trait;
use protocol::{AudioSource, ClipboardBundle, ClipboardFile, InputEvent};
use std::error::Error;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Macos,
    Windows,
    Linux,
    Other,
}

impl PlatformKind {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameHandleKind {
    Unknown,
    CpuMemory,
    MacosCvPixelBuffer,
    MacosIoSurface,
    WindowsD3D11Texture,
    WindowsD3D12Resource,
    LinuxDmaBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPixelFormat {
    Unknown,
    Bgra8,
    Rgba8,
    Nv12,
    P010,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleFormat {
    F32,
    I16,
    U16,
}

/// Abstract representation of a hardware video frame.
/// - macOS: May wrap an IOSurface or CVPixelBuffer.
/// - Windows: May wrap an ID3D11Texture2D.
/// - Linux: May wrap a DMA-BUF file descriptor.
pub trait VideoFrame: Send + Sync {
    // Add methods if needed, for instance getting resolution, format, etc.
    fn width(&self) -> u32;
    fn height(&self) -> u32;

    fn handle_kind(&self) -> VideoFrameHandleKind {
        VideoFrameHandleKind::Unknown
    }

    fn pixel_format(&self) -> VideoPixelFormat {
        VideoPixelFormat::Unknown
    }
}

#[async_trait]
pub trait VideoCapturer {
    type Frame: VideoFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Capture the next available frame.
    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait VideoEncoder {
    type Frame: VideoFrame;

    /// Submit a hardware frame to the encoder pipeline.
    async fn submit_frame(
        &mut self,
        frame: Self::Frame,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Pull encoded NAL units from the encoder pipeline. Blocks until a packet is ready.
    async fn pull_encoded(&mut self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait VideoDecoder {
    type Frame: VideoFrame;

    /// Decode a compressed bitstream (e.g. H.265 NAL units) back into a hardware frame.
    async fn decode(&mut self, data: &[u8]) -> Result<Self::Frame, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait VideoRenderer {
    type Frame: VideoFrame;

    /// Initialize or re-configure the renderer (e.g., handling window resizes).
    async fn configure(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Render a decoded hardware frame to the output surface.
    /// Implementations should handle zero-copy conversions from platform-specific
    /// hardware frames (IOSurface, DXGI, DMA-BUF) into Graphics API textures (Metal, Vulkan, DX12).
    async fn render(&mut self, frame: Self::Frame) -> Result<(), Box<dyn Error + Send + Sync>>;
}

/// Abstract representation of an audio frame.
pub trait AudioFrame: Send + Sync {
    fn samples(&self) -> &[f32];
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;

    fn source(&self) -> AudioSource {
        AudioSource::RemoteMicrophone
    }

    fn sample_format(&self) -> AudioSampleFormat {
        AudioSampleFormat::F32
    }
}

#[async_trait]
pub trait AudioCapturer {
    type Frame: AudioFrame;

    async fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>>;

    /// Capture the next available audio frame.
    async fn capture_frame(&mut self) -> Result<Self::Frame, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait AudioEncoder {
    type Frame: AudioFrame;

    /// Encode an audio frame into a compressed bitstream (e.g. Opus packets).
    async fn encode(&mut self, frame: Self::Frame)
    -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait AudioDecoder {
    type Frame: AudioFrame;

    /// Decode a compressed bitstream (e.g. Opus packets) back into an audio frame.
    async fn decode(&mut self, data: &[u8]) -> Result<Self::Frame, Box<dyn Error + Send + Sync>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipboardBackendCapabilities {
    pub text: bool,
    pub image: bool,
    pub file_references: bool,
    pub file_bytes: bool,
}

#[async_trait]
pub trait ClipboardProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::current()
    }

    fn capabilities(&self) -> ClipboardBackendCapabilities;

    async fn read_clipboard(
        &mut self,
        policy: ClipboardSyncPolicy,
    ) -> Result<Option<ClipboardBundle>, Box<dyn Error + Send + Sync>>;

    async fn write_clipboard(
        &mut self,
        bundle: &ClipboardBundle,
        policy: ClipboardSyncPolicy,
    ) -> Result<(), Box<dyn Error + Send + Sync>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFileReference {
    pub path: PathBuf,
}

impl ClipboardFileReference {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
pub trait ClipboardFileReferenceProvider {
    fn platform(&self) -> PlatformKind {
        PlatformKind::current()
    }

    async fn read_clipboard_file_references(
        &mut self,
    ) -> Result<Vec<ClipboardFileReference>, Box<dyn Error + Send + Sync>>;

    async fn write_clipboard_file_references(
        &mut self,
        references: &[ClipboardFileReference],
    ) -> Result<(), Box<dyn Error + Send + Sync>>;
}

#[async_trait]
pub trait ClipboardFileStore {
    async fn load_clipboard_file(
        &self,
        path: &Path,
    ) -> Result<ClipboardFile, Box<dyn Error + Send + Sync>>;

    async fn materialize_clipboard_file(
        &self,
        file: &ClipboardFile,
        target_dir: &Path,
    ) -> Result<PathBuf, Box<dyn Error + Send + Sync>>;
}

pub trait InputInjector {
    fn inject_input(&self, event: InputEvent) -> Result<(), Box<dyn Error + Send + Sync>>;
    fn release_all_input(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyVideoFrame;

    impl VideoFrame for DummyVideoFrame {
        fn width(&self) -> u32 {
            640
        }

        fn height(&self) -> u32 {
            480
        }
    }

    struct DummyAudioFrame {
        samples: Vec<f32>,
    }

    impl AudioFrame for DummyAudioFrame {
        fn samples(&self) -> &[f32] {
            &self.samples
        }

        fn sample_rate(&self) -> u32 {
            48_000
        }

        fn channels(&self) -> u16 {
            2
        }
    }

    #[test]
    fn platform_kind_current_matches_compile_target() {
        #[cfg(target_os = "macos")]
        assert_eq!(PlatformKind::current(), PlatformKind::Macos);

        #[cfg(target_os = "windows")]
        assert_eq!(PlatformKind::current(), PlatformKind::Windows);

        #[cfg(target_os = "linux")]
        assert_eq!(PlatformKind::current(), PlatformKind::Linux);
    }

    #[test]
    fn media_trait_defaults_are_platform_neutral() {
        let video = DummyVideoFrame;
        assert_eq!(video.handle_kind(), VideoFrameHandleKind::Unknown);
        assert_eq!(video.pixel_format(), VideoPixelFormat::Unknown);

        let audio = DummyAudioFrame {
            samples: vec![0.0, 1.0],
        };
        assert_eq!(audio.sample_format(), AudioSampleFormat::F32);
        assert_eq!(audio.source(), AudioSource::RemoteMicrophone);
    }

    #[test]
    fn clipboard_capabilities_default_to_none() {
        assert_eq!(
            ClipboardBackendCapabilities::default(),
            ClipboardBackendCapabilities {
                text: false,
                image: false,
                file_references: false,
                file_bytes: false,
            }
        );
    }
}
