use async_trait::async_trait;
use std::error::Error;

/// Abstract representation of a hardware video frame.
/// - macOS: May wrap an IOSurface or CVPixelBuffer.
/// - Windows: May wrap an ID3D11Texture2D.
/// - Linux: May wrap a DMA-BUF file descriptor.
pub trait VideoFrame: Send + Sync {
    // Add methods if needed, for instance getting resolution, format, etc.
    fn width(&self) -> u32;
    fn height(&self) -> u32;
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
