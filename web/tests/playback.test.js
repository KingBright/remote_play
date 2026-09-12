import test from 'node:test';
import assert from 'node:assert/strict';
import { RemotePlayWebClient } from '../src/client.js';
import { WebCodecsPlayer, isHevcKeyFrame } from '../src/decoder.js';
import { WebAudioPlayer } from '../src/audio.js';
import { canvasPoint } from '../src/input.js';

function environment(t) {
  const env = { sockets: [], decoders: [], intervals: new Map(), timeouts: new Map(), now: 0 };
  let nextId = 0;
  const globals = {
    performance: { now: () => env.now },
    setInterval: fn => { env.intervals.set(++nextId, fn); return nextId; },
    clearInterval: id => env.intervals.delete(id),
    setTimeout: fn => { env.timeouts.set(++nextId, fn); return nextId; },
    clearTimeout: id => env.timeouts.delete(id),
    EncodedVideoChunk: class { constructor(data) { Object.assign(this, data); } },
    VideoDecoder: class {
      static async isConfigSupported() { return { supported: true }; }
      constructor(callbacks) {
        this.callbacks = callbacks;
        this.decodeQueueSize = 0;
        this.chunks = [];
        this.state = 'unconfigured';
        env.decoders.push(this);
      }
      configure(config) { this.config = config; this.state = 'configured'; }
      decode(chunk) { this.chunks.push(chunk); this.decodeQueueSize++; }
      reset() { this.decodeQueueSize = 0; this.state = 'unconfigured'; }
      close() { this.state = 'closed'; }
    },
    WebSocket: class {
      static OPEN = 1;
      constructor(url) { this.url = url; this.readyState = 0; this.sent = []; env.sockets.push(this); }
      open() { this.readyState = 1; this.onopen?.(); }
      close() { this.readyState = 3; }
      send(data) { this.sent.push(JSON.parse(data)); }
    }
  };
  for (const [key, value] of Object.entries(globals)) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, key);
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value });
    t.after(() => {
      if (previous) Object.defineProperty(globalThis, key, previous);
      else delete globalThis[key];
    });
  }
  env.canvas = { width: 1920, height: 1080, getContext: () => ({ drawImage() {} }) };
  env.client = new RemotePlayWebClient(env.canvas);
  t.after(() => env.client.disconnect());
  return env;
}

test('connection failure is visible and never simulates streaming', async t => {
  const env = environment(t);
  let error;
  env.client.onError = value => { error = value; };
  await env.client.connect('https://invalid.example');
  assert.equal(env.client.state, 'error');
  assert.match(error, /ws:\/\//);
  assert.equal(env.client.telemetry.fps, null);
  assert.equal(env.sockets.length, 0);
  assert.equal(env.intervals.size, 0);
});

test('an open socket is not streaming; FPS comes from rendered frames', async t => {
  const env = environment(t);
  await env.client.connect();
  env.sockets[0].open();
  assert.equal(env.client.state, 'connected');
  let closedFrames = 0;
  for (let i = 0; i < 60; i++) {
    env.decoders[0].callbacks.output({ displayWidth: 1920, displayHeight: 1080, close() { closedFrames++; } });
  }
  env.now = 1000;
  for (const tick of env.intervals.values()) tick();
  assert.equal(env.client.state, 'streaming');
  assert.equal(env.client.telemetry.fps, 60);
  assert.equal(env.client.telemetry.latencyMs, null);
  assert.equal(closedFrames, 60);
  env.now = 2000;
  for (const tick of env.intervals.values()) tick();
  assert.equal(env.client.telemetry.fps, 0);
  assert.equal(env.client.state, 'connected');
});

test('reconnect ignores stale callbacks and releases timers, keys and decoders', async t => {
  const env = environment(t);
  await env.client.connect();
  const first = env.sockets[0];
  first.open();
  const staleClose = first.onclose;
  env.client.sendVirtualKey('ctrl', true);
  await env.client.connect();
  env.sockets[1].open();
  staleClose();
  assert.equal(env.client.state, 'connected');
  assert.equal(env.intervals.size, 1);
  assert.deepEqual(first.sent.map(item => item.pressed), [true, false]);
  assert.equal(env.decoders[0].state, 'closed');
  env.client.disconnect();
  assert.equal(env.intervals.size, 0);
  assert.equal(env.timeouts.size, 0);
  assert.equal(env.decoders[1].state, 'closed');
});

test('disconnect during codec capability detection cannot reopen the connection', async t => {
  const env = environment(t);
  let resolve;
  VideoDecoder.isConfigSupported = () => new Promise(done => { resolve = done; });
  const connecting = env.client.connect();
  env.client.disconnect();
  resolve({ supported: true });
  await connecting;
  assert.equal(env.client.state, 'disconnected');
  assert.equal(env.sockets.length, 0);
  assert.equal(env.decoders.length, 0);
});

test('disconnect cancels active touches with their original identifiers', async t => {
  const env = environment(t);
  await env.client.connect();
  env.sockets[0].open();
  env.client.sendTouch('Down', 0.25, 0.75, 9);
  env.client.disconnect();
  const [down, cancel] = env.sockets[0].sent;
  assert.deepEqual(cancel, { ...down, action: 'Cancel' });
  assert.equal(cancel.pointerId, 9);
  assert.equal(env.client.activeTouches.size, 0);
});

test('unsupported codec reports an error without changing the wire codec', async t => {
  const env = environment(t);
  VideoDecoder.isConfigSupported = async () => ({ supported: false });
  await env.client.connect();
  assert.equal(env.client.state, 'error');
  assert.equal(env.decoders.length, 0);
});

test('decode overload discards the damaged dependency chain until a keyframe', async t => {
  const env = environment(t);
  const player = new WebCodecsPlayer(env.canvas);
  await player.initDecoder();
  const bytes = new Uint8Array([0, 0, 0, 1, 38, 1]);
  assert.equal(isHevcKeyFrame(bytes), true);
  assert.equal(isHevcKeyFrame(new Uint8Array([0, 0, 1, 2, 1])), false);
  assert.equal(isHevcKeyFrame(new Uint8Array([0, 0, 1, 38])), false);
  assert.equal(player.decodeChunk(bytes, false, 0), false);
  assert.equal(player.decodeChunk(bytes, true, 1), true);
  env.decoders[0].decodeQueueSize = 3;
  assert.equal(player.decodeChunk(bytes, false, 2), false);
  assert.equal(player.needsKeyFrame, true);
  assert.equal(player.decodeChunk(bytes, true, 3), true);
  player.destroy();
});

test('drawing errors still close the native VideoFrame', async t => {
  const env = environment(t);
  const player = new WebCodecsPlayer({ getContext: () => ({ drawImage() { throw new Error('draw failed'); } }) });
  let released = false;
  let message;
  player.onError = error => { message = error.message; };
  await player.initDecoder();
  env.decoders[0].callbacks.output({ displayWidth: 2, displayHeight: 2, close() { released = true; } });
  assert.equal(released, true);
  assert.equal(message, 'draw failed');
  player.destroy();
});

test('connection timeout leaves no live resources', async t => {
  const env = environment(t);
  await env.client.connect();
  [...env.timeouts.values()][0]();
  assert.equal(env.client.state, 'error');
  assert.equal(env.timeouts.size, 0);
  assert.equal(env.client.player, null);
});

test('letterbox coordinates map to the actual remote image', () => {
  const rect = { left: 10, top: 20, width: 1000, height: 1000 };
  assert.deepEqual(canvasPoint(510, 520, rect, 1920, 1080), [0.5, 0.5]);
  assert.deepEqual(canvasPoint(10, 238.75, rect, 1920, 1080), [0, 0]);
  assert.ok(canvasPoint(510, 20, rect, 1920, 1080)[1] < 0);
  assert.equal(canvasPoint(0, 0, { ...rect, width: 0 }, 1920, 1080), null);
});

test('audio backlog stays bounded and close resets the playback clock', async t => {
  const sources = [];
  class AudioContext {
    constructor() { this.state = 'running'; this.currentTime = 0; }
    createBuffer(channels, length, rate) {
      return { duration: length / rate, getChannelData: () => new Float32Array(length) };
    }
    createBufferSource() {
      const source = { connect() {}, disconnect() {}, start() {}, stop() { this.stopped = true; } };
      sources.push(source);
      return source;
    }
    close() { return Promise.resolve(); }
  }
  const previous = globalThis.window;
  globalThis.window = { AudioContext };
  t.after(() => { if (previous === undefined) delete globalThis.window; else globalThis.window = previous; });
  const player = new WebAudioPlayer();
  await player.init();
  for (let i = 0; i < 20; i++) player.playPcmChunk(new Float32Array(1920));
  assert.ok(player.nextPlayTime <= 0.1);
  assert.ok(sources.some(source => source.stopped));
  player.close();
  assert.equal(player.nextPlayTime, 0);
  assert.equal(player.sources.size, 0);
  assert.ok(sources.every(source => source.stopped));
});
