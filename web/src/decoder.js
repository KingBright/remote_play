/**
 * Low latency HEVC Annex B playback. Each message must contain a complete
 * access unit; the native relay's multiplexed protocol needs a separate adapter.
 */
export class WebCodecsPlayer {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
    this.decoder = null;
    this.isConfigured = false;
    this.closed = false;
    this.needsKeyFrame = true;
    this.onFrame = null;
    this.onError = null;
    this.droppedFrames = 0;
  }

  async initDecoder() {
    if (typeof VideoDecoder === 'undefined') {
      throw new Error('This browser does not support WebCodecs. Use a supported browser over HTTPS or localhost.');
    }
    if (!this.ctx) throw new Error('Canvas rendering is unavailable.');
    const config = {
      codec: 'hev1.1.6.L120.90',
      optimizeForLatency: true,
      hardwareAcceleration: 'prefer-hardware'
    };
    const support = await VideoDecoder.isConfigSupported(config);
    if (this.closed) return;
    if (!support.supported) throw new Error('HEVC decoding is unavailable in this browser.');
    this.config = config;

    this.decoder = new VideoDecoder({
      output: (frame) => {
        try {
          if (this.closed) return;
          if (this.canvas.width !== frame.displayWidth || this.canvas.height !== frame.displayHeight) {
            this.canvas.width = frame.displayWidth;
            this.canvas.height = frame.displayHeight;
          }
          this.ctx.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
          this.onFrame?.();
        } catch (error) {
          this.onError?.(error);
        } finally {
          frame.close();
        }
      },
      error: (e) => {
        this.isConfigured = false;
        this.onError?.(e);
      }
    });

    this.decoder.configure(config);
    this.isConfigured = true;
  }

  decodeChunk(chunkBytes, isKeyFrame, timestampUs) {
    if (!this.decoder || !this.isConfigured || this.closed) return false;

    try {
      if (this.decoder.decodeQueueSize >= 3) {
        this.droppedFrames += this.decoder.decodeQueueSize;
        this.decoder.reset();
        this.decoder.configure(this.config);
        this.needsKeyFrame = true;
      }
      if (this.needsKeyFrame && !isKeyFrame) {
        this.droppedFrames++;
        return false;
      }
      const chunk = new EncodedVideoChunk({
        type: isKeyFrame ? 'key' : 'delta',
        timestamp: timestampUs,
        data: chunkBytes
      });
      this.decoder.decode(chunk);
      this.needsKeyFrame = false;
      return true;
    } catch (err) {
      this.onError?.(err);
      return false;
    }
  }

  destroy() {
    this.closed = true;
    this.isConfigured = false;
    if (this.decoder) {
      try {
        this.decoder.close();
      } catch (_) {}
      this.decoder = null;
    }
  }
}

export function isHevcKeyFrame(bytes) {
  for (let i = 0; i + 4 < bytes.length; i++) {
    if (bytes[i] !== 0 || bytes[i + 1] !== 0) continue;
    const start = bytes[i + 2] === 1 ? i + 3
      : bytes[i + 2] === 0 && bytes[i + 3] === 1 ? i + 4 : -1;
    if (start < 0 || start + 1 >= bytes.length) continue;
    const nalType = (bytes[start] >> 1) & 0x3f;
    if (nalType >= 16 && nalType <= 23) return true;
  }
  return false;
}
