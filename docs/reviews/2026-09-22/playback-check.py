"""Local macOS decoded-video check, with optional deterministic video loss.

Build the host and client playback_probe example first. Playback is muted to
avoid same-machine feedback. This checks decode, not display scanout or sound.
"""
import argparse
import os
from pathlib import Path
import socket
import subprocess
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("label")
parser.add_argument("--bin-dir", type=Path, default=Path("/tmp/remote-play-review-target/debug"))
parser.add_argument("--output", type=Path, default=Path("/tmp/remote-play-av-20260922"))
parser.add_argument("--seconds", type=int, default=15)
parser.add_argument("--host-port", type=int, default=39473)
parser.add_argument("--drop-at", type=float, default=-1)
parser.add_argument("--drop-duration", type=float, default=0.1)
parser.add_argument("--pause-faults", action="store_true", help="Drop first pause request/ACK; replay old pause after resume")
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
host_addr = ("127.0.0.1", args.host_port)
proxy = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
proxy.bind(("127.0.0.1", 0))
proxy.settimeout(0.1)
stop = threading.Event()
loss = {"frames": 0, "datagrams": 0}
media_datagrams = {}
pause_faults = {"request_dropped": 0, "ack_dropped": 0, "stale_replayed": 0}
start_streams = 0


def forward():
    global start_streams
    client_addr = None
    started = None
    dropped_ids = set()
    old_pause = None
    replay_at = None
    while not stop.is_set():
        if replay_at is not None and time.monotonic() >= replay_at:
            proxy.sendto(old_pause, host_addr)
            pause_faults["stale_replayed"] += 1
            replay_at = None
        try:
            data, source = proxy.recvfrom(65536)
        except socket.timeout:
            continue
        if source != host_addr:
            client_addr = source
            if started is None:
                started = time.monotonic()
            if data[:5] == b"\x02\x05\x00\x00\x00":
                start_streams += 1
            # This harness uses the unencrypted local bincode control protocol.
            # Appended variants 17/18 contain session u32, revision u64, bool.
            if args.pause_faults and len(data) == 18 and data[:5] == b"\x02\x11\x00\x00\x00":
                if data[-1] == 1:
                    old_pause = data
                    if not pause_faults["request_dropped"]:
                        pause_faults["request_dropped"] += 1
                        continue
                elif old_pause is not None and not pause_faults["stale_replayed"]:
                    replay_at = time.monotonic() + 1
            proxy.sendto(data, host_addr)
            continue
        if client_addr is None:
            continue
        if args.pause_faults and len(data) == 18 and data[:5] == b"\x02\x12\x00\x00\x00" and data[-1] == 1 and not pause_faults["ack_dropped"]:
            pause_faults["ack_dropped"] += 1
            continue
        elapsed = time.monotonic() - started
        if data[0] == 5 or (data[0] == 3 and len(data) > 1 and data[1] == 5):
            second = int(elapsed)
            media_datagrams[second] = media_datagrams.get(second, 0) + 1
        in_loss = args.drop_at <= elapsed < args.drop_at + args.drop_duration
        drop = False
        if data[0] == 3 and len(data) >= 13 and data[1] == 5:
            ident = data[2:6]
            index = int.from_bytes(data[6:8], "big")
            # Compact media header kind is byte 2; video is kind 1.
            if index == 0 and data[12] == 1 and in_loss:
                dropped_ids.add(ident)
                loss["frames"] += 1
            drop = ident in dropped_ids
        elif data[0] == 5 and len(data) >= 4 and data[3] == 1 and in_loss:
            drop = True
            loss["frames"] += 1
        if drop:
            loss["datagrams"] += 1
        else:
            proxy.sendto(data, client_addr)


env = os.environ.copy()
env.update(
    REMOTE_PLAY_HEADLESS="1", REMOTE_PLAY_DISCOVERY="0", REMOTE_PLAY_P2P="0",
    REMOTE_PLAY_RELAY="0", REMOTE_PLAY_CLIENT_RECEIVER="0",
    REMOTE_PLAY_CLIPBOARD_SYNC="0", REMOTE_PLAY_FILE_TRANSFER="0",
    REMOTE_PLAY_TALKBACK="0", REMOTE_PLAY_SYSTEM_AUDIO="1",
    REMOTE_PLAY_DEVICE_GROUP_DIR=str(args.output / "config" / "mesh"),
    REMOTE_PLAY_HOST_BIND_ADDR=f"{host_addr[0]}:{host_addr[1]}",
    REMOTE_PLAY_PROBE_SAME_HOST="1", REMOTE_PLAY_PROBE_SECONDS=str(args.seconds),
    REMOTE_PLAY_PROBE_HOST=f"127.0.0.1:{proxy.getsockname()[1]}",
)
host_log = args.output / f"{args.label}-host.log"
thread = threading.Thread(target=forward)
with host_log.open("w") as log:
    host = subprocess.Popen([str(args.bin_dir / "remote_play")], env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(100):
            current_log = host_log.read_text()
            if "Unified passive host service stopped:" in current_log:
                raise RuntimeError(current_log)
            if "Listening for ControlMessages" in current_log:
                break
            if host.poll() is not None:
                raise RuntimeError("Host stopped before listening")
            time.sleep(0.1)
        else:
            raise RuntimeError("Host did not listen within ten seconds")
        thread.start()
        playback_log = args.output / f"{args.label}-playback.log"
        with playback_log.open("w") as out:
            result = subprocess.run(
                [str(args.bin_dir / "examples" / "playback_probe")], env=env,
                stdout=out, stderr=subprocess.STDOUT, timeout=args.seconds + 15,
            )
        print(playback_log.read_text())
        print(f"injected_loss={loss}")
        print(f"start_streams={start_streams} pause_faults={pause_faults}")
        if start_streams != 1:
            raise RuntimeError("Expected one session throughout the probe")
        if args.pause_faults and any(count != 1 for count in pause_faults.values()):
            raise RuntimeError("Pause fault injection did not complete")
        if "REMOTE_PLAY_PROBE_PAUSE_AT" in env:
            print(f"host_media_datagrams_by_second={media_datagrams}")
            start = int(env["REMOTE_PLAY_PROBE_PAUSE_AT"]) + 1
            end = int(env["REMOTE_PLAY_PROBE_PAUSE_AT"]) + int(env.get("REMOTE_PLAY_PROBE_PAUSE_SECONDS", "20"))
            if any(media_datagrams.get(second, 0) for second in range(start, end)):
                raise RuntimeError("Host kept sending media while paused (after one-second grace)")
            if args.pause_faults and not all(media_datagrams.get(second, 0) for second in range(end + 2, args.seconds - 1)):
                raise RuntimeError("Media stopped after replaying a stale pause command")
        if result.returncode:
            raise SystemExit(result.returncode)
        if args.drop_at >= 0 and loss["frames"] == 0:
            raise RuntimeError("Loss scenario failed to inject video loss")
    finally:
        stop.set()
        if thread.ident is not None:
            thread.join(timeout=2)
        proxy.close()
        host.terminate()
        try:
            host.wait(timeout=5)
        except subprocess.TimeoutExpired:
            host.kill()
            host.wait()
