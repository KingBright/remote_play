import { WebCodecsPlayer, isHevcKeyFrame } from './decoder.js';

export class RemotePlayWebClient {
  constructor(canvasElement) {
    this.canvas = canvasElement;
    this.player = null;
    this.ws = null;
    this.state = 'disconnected';
    this.onStateChange = null;
    this.onTelemetry = null;
    this.onError = null;
    this.generation = 0;
    this.telemetryTimer = null;
    this.connectTimer = null;
    this.frameTimer = null;
    this.pressedKeys = new Set();
    this.activeTouches = new Map();
    this.telemetry = {
      fps: null,
      latencyMs: null,
      videoBitrate: null,
      audioBitrate: null,
      controlBitrate: null
    };
  }

  async connect(wsUrl = 'ws://localhost:9002/relay') {
    this.disconnect();
    const generation = ++this.generation;
    this.setState('connecting');
    this.receivedBytes = 0;
    this.renderedFrames = 0;
    try {
      const url = new URL(wsUrl);
      if (!['ws:', 'wss:'].includes(url.protocol)) throw new Error('Enter a ws:// or wss:// endpoint.');
      const player = new WebCodecsPlayer(this.canvas);
      this.player = player;
      player.onError = (error) => this.fail(error, generation);
      player.onFrame = () => {
        if (generation !== this.generation) return;
        this.renderedFrames++;
        clearTimeout(this.frameTimer);
        this.setState('streaming');
      };
      await player.initDecoder();
      if (generation !== this.generation) return;
      const ws = new WebSocket(url.href);
      this.ws = ws;
      ws.binaryType = 'arraybuffer';
      this.connectTimer = setTimeout(() => this.fail(new Error('Connection timed out.'), generation), 10000);
      ws.onopen = () => {
        if (generation !== this.generation) return;
        clearTimeout(this.connectTimer);
        this.setState('connected');
        this.startTelemetryLoop();
        this.frameTimer = setTimeout(() => this.fail(new Error('No decodable video received. This client requires an HEVC Annex B WebSocket gateway; the native relay needs a protocol adapter.'), generation), 10000);
      };

      ws.onmessage = (event) => {
        if (generation !== this.generation) return;
        if (event.data instanceof ArrayBuffer) {
          const bytes = new Uint8Array(event.data);
          this.receivedBytes += bytes.byteLength;
          player.decodeChunk(bytes, isHevcKeyFrame(bytes), Math.round(performance.now() * 1000));
        }
      };

      ws.onerror = () => this.fail(new Error('WebSocket connection failed. Check the endpoint and its TLS settings.'), generation);
      ws.onclose = () => {
        if (generation === this.generation) this.fail(new Error('The remote connection closed. Reconnect to continue.'), generation);
      };
    } catch (err) {
      this.fail(err, generation);
    }
  }

  setState(state) {
    if (this.state === state) return;
    this.state = state;
    this.onStateChange?.(state);
  }

  fail(error, generation) {
    if (generation !== this.generation) return;
    this.disconnect();
    this.setState('error');
    this.onError?.(error.message || String(error));
  }

  disconnect() {
    for (const [id, coords] of this.activeTouches) this.sendTouch('Cancel', ...coords, id);
    this.activeTouches.clear();
    this.releaseKeys();
    ++this.generation;
    clearInterval(this.telemetryTimer);
    clearTimeout(this.connectTimer);
    clearTimeout(this.frameTimer);
    this.telemetryTimer = this.connectTimer = this.frameTimer = null;
    if (this.ws) {
      this.ws.onopen = this.ws.onmessage = this.ws.onclose = this.ws.onerror = null;
      try { this.ws.close(); } catch (_) {}
      this.ws = null;
    }
    this.player?.destroy();
    this.player = null;
    for (const key of Object.keys(this.telemetry)) this.telemetry[key] = null;
    this.onTelemetry?.(this.telemetry);
    this.setState('disconnected');
  }

  sendTouch(action, normX, normY, pointerId = 0) {
    if (!Number.isFinite(normX) || !Number.isFinite(normY)) return;
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      const payload = JSON.stringify({
        type: 'Touch',
        action,
        pointerId,
        normX: Math.max(0, Math.min(1, normX)),
        normY: Math.max(0, Math.min(1, normY))
      });
      try { this.ws.send(payload); } catch (_) { return; }
      if (action === 'Down' || action === 'Move') this.activeTouches.set(pointerId, [normX, normY]);
      else this.activeTouches.delete(pointerId);
    }
  }

  sendVirtualKey(keyName, pressed) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      const payload = JSON.stringify({
        type: 'Key',
        keyName,
        pressed
      });
      try { this.ws.send(payload); } catch (_) { return; }
      if (pressed) this.pressedKeys.add(keyName);
      else this.pressedKeys.delete(keyName);
    }
  }

  releaseKeys() {
    for (const key of this.pressedKeys) this.sendVirtualKey(key, false);
    this.pressedKeys.clear();
  }

  startTelemetryLoop() {
    clearInterval(this.telemetryTimer);
    let lastTime = performance.now();
    let lastFrames = this.renderedFrames;
    let lastBytes = this.receivedBytes;
    this.telemetryTimer = setInterval(() => {
      const now = performance.now();
      const elapsed = (now - lastTime) / 1000;
      if (elapsed <= 0) return;
      this.telemetry.fps = Math.round((this.renderedFrames - lastFrames) / elapsed);
      this.telemetry.videoBitrate = `${((this.receivedBytes - lastBytes) * 8 / elapsed / 1e6).toFixed(2)} Mbps`;
      if (this.renderedFrames === lastFrames && this.state === 'streaming') this.setState('connected');
      lastTime = now;
      lastFrames = this.renderedFrames;
      lastBytes = this.receivedBytes;
      this.onTelemetry?.(this.telemetry);
    }, 1000);
  }
}
