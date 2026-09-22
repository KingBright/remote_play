"""Local UDP observer for the Android emulator's media-only pause check.

Run with the freshly built release host. Connect Android to 10.0.2.2:39475,
background/foreground the viewer, then interrupt this harness to save counts.
Only the unencrypted local test protocol is decoded here.
"""
import json
import os
from pathlib import Path
import socket
import subprocess
import time

out = Path("/tmp/remote-play-av-20260922")
out.mkdir(exist_ok=True)
env = os.environ.copy()
env.update(REMOTE_PLAY_HEADLESS="1", REMOTE_PLAY_DISCOVERY="0", REMOTE_PLAY_P2P="0",
           REMOTE_PLAY_RELAY="0", REMOTE_PLAY_CLIENT_RECEIVER="0",
           REMOTE_PLAY_CLIPBOARD_SYNC="0", REMOTE_PLAY_FILE_TRANSFER="0",
           REMOTE_PLAY_TALKBACK="0", REMOTE_PLAY_SYSTEM_AUDIO="1",
           REMOTE_PLAY_DEVICE_GROUP_DIR=str(out / "android-mesh"),
           REMOTE_PLAY_HOST_BIND_ADDR="127.0.0.1:39474")
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("127.0.0.1", 39475))
sock.settimeout(0.1)
host_addr = ("127.0.0.1", 39474)
started = time.monotonic()
events = []
media = {}
wire = {}
peer = None
with (out / "android-pause-host.log").open("w") as log:
    host = subprocess.Popen(["/tmp/remote-play-review-target/release/remote_play"],
                            env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        while time.monotonic() - started < 900:
            try:
                data, source = sock.recvfrom(65536)
            except socket.timeout:
                continue
            elapsed = round(time.monotonic() - started, 3)
            to_host = source != host_addr
            second = int(elapsed)
            counters = wire.setdefault(second, {"client_packets": 0, "host_packets": 0, "udp_payload_bytes": 0})
            counters["client_packets" if to_host else "host_packets"] += 1
            counters["udp_payload_bytes"] += len(data)
            if to_host:
                peer = source
            elif data[0] == 5 or (data[0] == 3 and len(data) > 1 and data[1] == 5):
                second = int(elapsed)
                media[second] = media.get(second, 0) + 1
            if data[0] == 2 and len(data) >= 5:
                tag = int.from_bytes(data[1:5], "little")
                if tag in (5, 6, 8, 10, 17, 18):
                    event = {"seconds": elapsed, "to_host": to_host, "tag": tag}
                    if tag == 5 and len(data) == 25:
                        event["session"] = int.from_bytes(data[-4:], "little")
                    if tag in (17, 18) and len(data) == 18:
                        event.update(session=int.from_bytes(data[5:9], "little"),
                                     revision=int.from_bytes(data[9:17], "little"), paused=bool(data[-1]))
                    events.append(event)
                    if tag != 8 and tag != 10:
                        print(json.dumps(event), flush=True)
            if to_host or peer is not None:
                sock.sendto(data, host_addr if to_host else peer)
    except KeyboardInterrupt:
        pass
    finally:
        sock.close()
        host.terminate()
        try:
            host.wait(timeout=5)
        except subprocess.TimeoutExpired:
            host.kill()
            host.wait()
        (out / "android-pause-wire.json").write_text(json.dumps({"events": events, "media_datagrams_by_second": media, "wire_by_second": wire}, indent=2))
