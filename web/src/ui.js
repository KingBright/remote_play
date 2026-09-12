import { RemotePlayWebClient } from './client.js';
import { canvasPoint } from './input.js';

document.addEventListener('DOMContentLoaded', () => {
  const canvas = document.getElementById('remote-canvas');
  const client = new RemotePlayWebClient(canvas);

  const islandTitle = document.getElementById('island-title');
  const telemetryBadge = document.getElementById('telemetry-badge');
  const drawerOverlay = document.getElementById('drawer-overlay');
  const drawerTrigger = document.getElementById('drawer-trigger');
  const closeDrawerBtn = document.getElementById('close-drawer-btn');
  const btnDisconnect = document.getElementById('btn-disconnect');
  const endpoint = document.getElementById('endpoint');
  const status = document.getElementById('connection-status');
  const connectBtn = document.getElementById('btn-connect');
  const activePointers = new Map();

  // 状态与遥测更新
  client.onStateChange = (state) => {
    const labels = { disconnected: 'RemotePlay Ready', connecting: 'Connecting…',
      connected: 'Waiting for video…', streaming: 'RemotePlay Live', error: 'Connection failed' };
    islandTitle.textContent = labels[state];
    telemetryBadge.hidden = !['connected', 'streaming'].includes(state);
    btnDisconnect.disabled = !['connecting', 'connected', 'streaming'].includes(state);
    connectBtn.disabled = state === 'connecting';
    document.querySelectorAll('.status-dot').forEach(dot => {
      dot.style.background = state === 'streaming' ? 'var(--color-accent-emerald)' : '#80848b';
    });
    if (state === 'disconnected' || state === 'error') activePointers.clear();
    if (state === 'connected') status.textContent = 'Connected. Waiting for a decodable video frame…';
    if (state === 'streaming') status.textContent = 'Live video received. Frame rate is measured from rendered frames.';
  };

  client.onTelemetry = (stats) => {
    telemetryBadge.textContent = `${stats.fps ?? '—'} FPS · ${stats.latencyMs == null ? 'Latency unavailable' : `${stats.latencyMs}ms`}`;
    document.getElementById('video-bitrate').textContent = stats.videoBitrate ?? '—';
  };
  client.onError = (message) => { status.textContent = message; };
  const connect = () => {
    status.textContent = 'Connecting to the video gateway…';
    client.connect(endpoint.value.trim());
  };
  connectBtn.addEventListener('click', connect);
  endpoint.addEventListener('keydown', event => { if (event.key === 'Enter') connect(); });

  // 抽屉开关
  drawerTrigger.addEventListener('click', () => {
    drawerOverlay.classList.add('open');
    endpoint.focus();
  });

  closeDrawerBtn.addEventListener('click', () => {
    drawerOverlay.classList.remove('open');
    drawerTrigger.querySelector('button').focus();
  });

  // 控制岛按钮交互
  btnDisconnect.addEventListener('click', () => {
    releaseInput();
    client.disconnect();
    status.textContent = 'Disconnected. Enter a gateway endpoint to reconnect.';
  });

  // 虚拟修饰键点击
  document.querySelectorAll('.key-pill').forEach((pill) => {
    const key = pill.getAttribute('data-key');
    pill.addEventListener('pointerdown', (event) => {
      event.preventDefault();
      pill.setPointerCapture(event.pointerId);
      client.sendVirtualKey(key, true);
    });
    for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) {
      pill.addEventListener(name, () => {
        if (client.pressedKeys.has(key)) client.sendVirtualKey(key, false);
      });
    }
    pill.addEventListener('keydown', event => {
      if ([' ', 'Enter'].includes(event.key) && !event.repeat) client.sendVirtualKey(key, true);
    });
    pill.addEventListener('keyup', event => {
      if ([' ', 'Enter'].includes(event.key)) client.sendVirtualKey(key, false);
    });
    pill.addEventListener('blur', () => {
      if (client.pressedKeys.has(key)) client.sendVirtualKey(key, false);
    });
  });

  canvas.style.touchAction = 'none';
  const pointer = (event, action) => {
    if (action !== 'Down' && !activePointers.has(event.pointerId)) return;
    const rect = canvas.getBoundingClientRect();
    const coords = canvasPoint(event.clientX, event.clientY, rect, canvas.width, canvas.height);
    if (!coords || (action === 'Down' && coords.some(value => value < 0 || value > 1))) return;
    client.sendTouch(action, ...coords, event.pointerId);
    if (action === 'Up' || action === 'Cancel') activePointers.delete(event.pointerId);
    else activePointers.set(event.pointerId, coords);
  };
  canvas.addEventListener('pointerdown', event => {
    canvas.setPointerCapture(event.pointerId);
    pointer(event, 'Down');
  });
  canvas.addEventListener('pointermove', event => pointer(event, 'Move'));
  canvas.addEventListener('pointerup', event => pointer(event, 'Up'));
  for (const name of ['pointercancel', 'lostpointercapture']) {
    canvas.addEventListener(name, event => pointer(event, 'Cancel'));
  }
  function releaseInput() {
    for (const [id, coords] of activePointers) client.sendTouch('Cancel', ...coords, id);
    activePointers.clear();
    client.releaseKeys();
  }
  window.addEventListener('blur', releaseInput);
  document.addEventListener('visibilitychange', () => { if (document.hidden) releaseInput(); });
  window.addEventListener('pagehide', () => { releaseInput(); client.disconnect(); });
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      releaseInput();
      drawerOverlay.classList.remove('open');
    }
  });

  // 绘制初始测试图案
  const ctx = canvas.getContext('2d');
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
  ctx.fillStyle = '#0B0D10';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  client.onStateChange('disconnected');
  client.onTelemetry(client.telemetry);
  drawerOverlay.classList.add('open');
});
