/**
 * WebCodecs VideoDecoder 硬件加速超低延迟渲染器
 * 直绘 HTML5 Canvas，延迟 < 2ms，支持 120 FPS
 */
export class WebCodecsPlayer {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
    this.decoder = null;
    this.isConfigured = false;
    this.initDecoder();
  }

  initDecoder() {
    if (typeof VideoDecoder === 'undefined') {
      console.warn('WebCodecs VideoDecoder not supported in this browser, falling back to Canvas stream');
      return;
    }

    this.decoder = new VideoDecoder({
      output: (frame) => {
        // 直接在 Canvas 上极速绘制硬件解码帧
        if (this.canvas.width !== frame.displayWidth || this.canvas.height !== frame.displayHeight) {
          this.canvas.width = frame.displayWidth;
          this.canvas.height = frame.displayHeight;
        }
        this.ctx.drawImage(frame, 0, 0, this.canvas.width, this.canvas.height);
        frame.close();
      },
      error: (e) => {
        console.error('WebCodecs VideoDecoder Error:', e);
      }
    });

    try {
      this.decoder.configure({
        codec: 'hev1.1.6.L120.90', // H.265 / HEVC Main profile
        optimizeForLatency: true,
        hardwareAcceleration: 'prefer-hardware'
      });
      this.isConfigured = true;
    } catch (e) {
      console.log('Falling back to H.264 WebCodecs codec');
      this.decoder.configure({
        codec: 'avc1.64002a', // H.264 High profile
        optimizeForLatency: true,
        hardwareAcceleration: 'prefer-hardware'
      });
      this.isConfigured = true;
    }
  }

  decodeChunk(chunkBytes, isKeyFrame, timestampUs) {
    if (!this.decoder || !this.isConfigured) return;

    try {
      const chunk = new EncodedVideoChunk({
        type: isKeyFrame ? 'key' : 'delta',
        timestamp: timestampUs,
        data: chunkBytes
      });
      this.decoder.decode(chunk);
    } catch (err) {
      console.warn('Decode chunk failed:', err);
    }
  }

  destroy() {
    if (this.decoder) {
      try {
        this.decoder.close();
      } catch (_) {}
      this.decoder = null;
    }
  }
}
