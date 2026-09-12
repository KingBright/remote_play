/**
 * WebAudio 极低延迟音频流播放器
 */
export class WebAudioPlayer {
  constructor() {
    this.ctx = null;
    this.nextPlayTime = 0;
    this.sources = new Set();
  }

  async init() {
    if (!this.ctx) {
      const AudioCtx = window.AudioContext || window.webkitAudioContext;
      if (!AudioCtx) throw new Error('Web Audio is unavailable in this browser.');
      this.ctx = new AudioCtx({ sampleRate: 48000, latencyHint: 'interactive' });
    }
    if (this.ctx.state === 'suspended') {
      await this.ctx.resume();
    }
  }

  playPcmChunk(pcmFloat32Array) {
    if (!this.ctx || this.ctx.state !== 'running' || pcmFloat32Array.length === 0
        || pcmFloat32Array.length % 2 !== 0) return;
    const currentTime = this.ctx.currentTime;
    // Keep queued playback within 100 ms, including the incoming chunk.
    if (this.nextPlayTime - currentTime + pcmFloat32Array.length / 96000 > 0.1) {
      for (const source of this.sources) source.stop();
      this.sources.clear();
      this.nextPlayTime = currentTime;
    }
    if (pcmFloat32Array.length / 96000 > 0.1) return;

    const buffer = this.ctx.createBuffer(2, pcmFloat32Array.length / 2, 48000);
    const leftChannel = buffer.getChannelData(0);
    const rightChannel = buffer.getChannelData(1);

    for (let i = 0; i < pcmFloat32Array.length / 2; i++) {
      leftChannel[i] = pcmFloat32Array[i * 2];
      rightChannel[i] = pcmFloat32Array[i * 2 + 1];
    }

    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.ctx.destination);
    this.sources.add(source);
    source.onended = () => {
      source.disconnect();
      this.sources.delete(source);
    };

    if (this.nextPlayTime < currentTime) {
      this.nextPlayTime = currentTime;
    }

    source.start(this.nextPlayTime);
    this.nextPlayTime += buffer.duration;
  }

  close() {
    this.nextPlayTime = 0;
    for (const source of this.sources) {
      try { source.stop(); } catch (_) {}
      source.disconnect();
    }
    this.sources.clear();
    if (this.ctx) {
      this.ctx.close().catch(() => {});
      this.ctx = null;
    }
  }
}
