import { RemotePlayWebClient } from './client.js';

document.addEventListener('DOMContentLoaded', () => {
  const canvas = document.getElementById('remote-canvas');
  const client = new RemotePlayWebClient(canvas);

  const islandTitle = document.getElementById('island-title');
  const telemetryBadge = document.getElementById('telemetry-badge');
  const drawerOverlay = document.getElementById('drawer-overlay');
  const drawerTrigger = document.getElementById('drawer-trigger');
  const closeDrawerBtn = document.getElementById('close-drawer-btn');
  const btnDisconnect = document.getElementById('btn-disconnect');
  const btnMic = document.getElementById('btn-mic');
  const btnClip = document.getElementById('btn-clip');
  const btnTouchMode = document.getElementById('btn-touch-mode');
  const modifierBar = document.getElementById('modifier-bar');

  let isTouchModeDirect = true;
  let micActive = false;
  let clipActive = true;

  // 状态与遥测更新
  client.onStateChange = (state) => {
    if (state === 'streaming') {
      islandTitle.textContent = 'Gaming Rig RTX 4090';
      telemetryBadge.style.display = 'inline-block';
    } else {
      islandTitle.textContent = 'RemotePlay Ready';
      telemetryBadge.style.display = 'none';
    }
  };

  client.onTelemetry = (stats) => {
    telemetryBadge.textContent = `${stats.fps} FPS · ${stats.latencyMs}ms`;
  };

  // 抽屉开关
  drawerTrigger.addEventListener('click', () => {
    drawerOverlay.classList.add('open');
  });

  closeDrawerBtn.addEventListener('click', () => {
    drawerOverlay.classList.remove('open');
  });

  // 控制岛按钮交互
  btnDisconnect.addEventListener('click', () => {
    client.disconnect();
  });

  btnMic.addEventListener('click', () => {
    micActive = !micActive;
    btnMic.classList.toggle('active', micActive);
  });

  btnClip.addEventListener('click', () => {
    clipActive = !clipActive;
    btnClip.classList.toggle('active', clipActive);
  });

  btnTouchMode.addEventListener('click', () => {
    isTouchModeDirect = !isTouchModeDirect;
    btnTouchMode.textContent = isTouchModeDirect ? 'TOUCH' : 'TRACKPAD';
    btnTouchMode.classList.toggle('active', isTouchModeDirect);
  });

  // 虚拟修饰键点击
  document.querySelectorAll('.key-pill').forEach((pill) => {
    const key = pill.getAttribute('data-key');
    pill.addEventListener('mousedown', () => {
      client.sendVirtualKey(key, true);
    });
    pill.addEventListener('mouseup', () => {
      client.sendVirtualKey(key, false);
    });
  });

  // 画布多点触控与手势监听
  canvas.addEventListener('touchstart', (e) => {
    e.preventDefault();
    if (e.touches.length > 0) {
      const touch = e.touches[0];
      const rect = canvas.getBoundingClientRect();
      const normX = (touch.clientX - rect.left) / rect.width;
      const normY = (touch.clientY - rect.top) / rect.height;
      client.sendTouch('Down', normX, normY, touch.identifier);
    }
  }, { passive: false });

  canvas.addEventListener('touchmove', (e) => {
    e.preventDefault();
    if (e.touches.length > 0) {
      const touch = e.touches[0];
      const rect = canvas.getBoundingClientRect();
      const normX = (touch.clientX - rect.left) / rect.width;
      const normY = (touch.clientY - rect.top) / rect.height;
      client.sendTouch('Move', normX, normY, touch.identifier);
    }
  }, { passive: false });

  canvas.addEventListener('touchend', (e) => {
    e.preventDefault();
    client.sendTouch('Up', 0, 0, 0);
  }, { passive: false });

  // 绘制初始测试图案
  const ctx = canvas.getContext('2d');
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
  ctx.fillStyle = '#0B0D10';
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  // 默认启动连接
  client.connect();
});
