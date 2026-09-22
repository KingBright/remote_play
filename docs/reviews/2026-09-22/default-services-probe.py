import os, subprocess, time, hashlib, json
from pathlib import Path
base = Path('/tmp/remote-play-review-20260922/default-services')
base.mkdir(exist_ok=True)
source = base / 'upload.bin'
source.write_bytes(os.urandom(1024 * 1024))
env = os.environ.copy()
for key in ['REMOTE_PLAY_FILE_TRANSFER', 'REMOTE_PLAY_FILE_CLIPBOARD', 'REMOTE_PLAY_HOST_SEND_FILE', 'REMOTE_PLAY_SEND_FILE', 'REMOTE_PLAY_SESSION_PSK', 'REMOTE_PLAY_REQUIRE_AUTH']:
    env.pop(key, None)
env.update(REMOTE_PLAY_HEADLESS='1', REMOTE_PLAY_P2P='0', REMOTE_PLAY_RELAY='0', REMOTE_PLAY_DISCOVERY='0', REMOTE_PLAY_CLIENT_RECEIVER='0', REMOTE_PLAY_CLIPBOARD_SYNC='0', REMOTE_PLAY_TALKBACK='0', REMOTE_PLAY_HOST_BIND_ADDR='127.0.0.1:49372', REMOTE_PLAY_DEVICE_GROUP_DIR=str(base/'config'/'mesh'), REMOTE_PLAY_FILE_RECEIVE_DIR=str(base/'host-received'), REMOTE_PLAY_SYSTEM_AUDIO='1')
bin_dir = Path('/tmp/remote-play-review-target/debug')
with (base/'host.log').open('w') as out:
    host = subprocess.Popen([str(bin_dir/'remote_play')], env=env, stdout=out, stderr=subprocess.STDOUT)
    try:
        for _ in range(100):
            if 'Listening for ControlMessages' in (base/'host.log').read_text(): break
            if host.poll() is not None: raise RuntimeError('host exited')
            time.sleep(0.1)
        else: raise RuntimeError('host readiness timed out')
        client_env = env.copy()
        client_env.update(REMOTE_PLAY_SMOKE_HOST_ADDR='127.0.0.1:49372', REMOTE_PLAY_SMOKE_SEND_FILE=str(source), REMOTE_PLAY_SMOKE_SECONDS='10', REMOTE_PLAY_SMOKE_RECEIVE_DIR=str(base/'client-received'), REMOTE_PLAY_EXPECT_DATA_PLANE_MEDIA='1', REMOTE_PLAY_EXPECT_AUDIO='1', REMOTE_PLAY_EXPECT_SYSTEM_AUDIO='1')
        with (base/'client.log').open('w') as log:
            result = subprocess.run([str(bin_dir/'examples'/'headless_smoke_client')], env=client_env, stdout=log, stderr=subprocess.STDOUT, timeout=25)
        received = base/'host-received'/'upload.bin'
        src_sha = hashlib.sha256(source.read_bytes()).hexdigest()
        dst_sha = hashlib.sha256(received.read_bytes()).hexdigest() if received.exists() else None
        report = dict(client_exit=result.returncode, default_file_transfer_env_unset=True, bytes=source.stat().st_size, source_sha256=src_sha, received_sha256=dst_sha, file_equal=src_sha==dst_sha)
        (base/'result.json').write_text(json.dumps(report, indent=2)+'\n')
        print(json.dumps(report))
        print((base/'client.log').read_text())
        if result.returncode or src_sha != dst_sha: raise SystemExit(1)
    finally:
        host.terminate()
        try: host.wait(timeout=5)
        except subprocess.TimeoutExpired:
            host.kill()
            host.wait()
