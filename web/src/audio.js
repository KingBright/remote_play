/**
 * WebAudio 极低延迟音频流播放器
 */
export class WebAudioPlayer {
  constructor() {
    this.ctx = null;
    this.nextPlayTime = 0;
  }

  init() {
    if (!this.ctx) {
      const AudioCtx = window.AudioContext || window.webkitAudioContext;
      this.ctx = new AudioCtx({ sampleRate: 48000, latencyHint: 'interactive' });
    }
    if (this.ctx.state === 'suspended') {
      this.ctx.resume();
    }
  }

  playPcmChunk(pcmFloat32Array) {
    if (!this.ctx) return;

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

    const currentTime = this.ctx.currentTime;
    if (this.nextPlayTime < currentTime) {
      this.nextPlayTime = currentTime;
    }

    source.start(this.nextPlayTime);
    this.nextPlayTime += buffer.duration;
  }

  close() {
    if (this.ctx) {
      this.ctx.close();
      this.ctx = null;
    }
  }
}
