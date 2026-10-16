#!/usr/bin/env python3
"""Verify native fixture effects separately from SDK enqueue/cleanup receipts."""
import argparse
import json
from pathlib import Path


def require(value, reason):
    if not value:
        raise ValueError(reason)


def verify(before, after, sender, backend_pid):
    require(type(backend_pid) is int and backend_pid > 0, "exact backend PID required")
    require(sender.get("requested_sequence") == "drag_scroll_abc_shift_a", "suite receipt required")
    require(sender.get("input_commands_enqueued") == 16 and sender.get("cleanup_complete") is True
            and sender.get("operation_succeeded") is True, "complete SDK sequence and cleanup required")
    require(after.get("pid") == before.get("pid") == sender.get("fixture_pid"), "fixture process changed")
    for state in (before, after):
        require(state["A"]["id"] == sender.get("fixture_window")
                and state["A"]["title"] == "RemotePlay scoped-window acceptance " + sender["fixture_nonce"],
                "exact fixture window/title required")
    start = before.get("input_events", [])
    events = after.get("input_events", [])
    # A bounded event ring may have rolled over: never invent the missing prefix.
    require(len(events) >= len(start) and events[:len(start)] == start, "native event prefix missing")
    events = events[len(start):]
    own = [e for e in events if e.get("source_pid") == backend_pid]
    require(own and all(e.get("window") == "A" for e in own), "event reached another window")
    require(all(a["monotonic_ns"] <= b["monotonic_ns"] for a,b in zip(own,own[1:])), "event clock regressed")
    kinds = [e["kind"] for e in own]
    require("mouse_down" in kinds and "mouse_drag" in kinds and "mouse_up" in kinds,
            "native drag sequence missing")
    down = kinds.index("mouse_down"); up = kinds.index("mouse_up", down)
    drag = [e for e in own[down+1:up] if e["kind"] == "mouse_drag"]
    require(len(drag) >= 2 and abs(drag[-1]["x"]-drag[0]["x"]) > 30, "native drag displacement missing")
    require(any(e["kind"] == "scroll" and (e["scroll_x"] or e["scroll_y"]) for e in own), "native wheel effect missing")
    downs = [e for e in own if e["kind"] == "key_down"]
    ups = [e for e in own if e["kind"] == "key_up"]
    require([e["key_code"] for e in downs] == [0,11,8,0]
            and [e["key_code"] for e in ups] == [0,11,8,0], "native key down/up sequence missing")
    require([(e["kind"],e["key_code"]) for e in own if e["kind"] in ("key_down","key_up")]
            == [(kind,code) for code in (0,11,8,0) for kind in ("key_down","key_up")], "native key pairing/order changed")
    require("".join(e["characters"] for e in downs) == "abcA", "native text or Shift+A effect missing")
    require(downs[-1]["flags"] & (1 << 17), "native Shift modifier missing")
    flags=[e for e in own if e["kind"] == "flags"]
    require(len(flags)>=2 and any(e["flags"] & (1 << 17) for e in flags)
            and not flags[-1]["flags"] & (1 << 17), "native modifier press/release missing")
    require(all(not after[label][key] for label in ("A","B") for key in ("held_keys","held_buttons")),
            "native presses remain held")
    require(after.get("text", {}).get("B", "") == before.get("text", {}).get("B", ""), "adjacent text changed")
    return {"kind":"native-synthetic-input-suite-effects", "verified":True,
            "fixture_pid":after["pid"], "window":after["A"]["id"], "backend_pid":backend_pid,
            "native_drag":True,"native_scroll":True,"native_text":"abcA","native_key_down_up":True,
            "held_state_empty":True,"host_per_event_ack_exposed":False,
            "boundary":"fixture received OS events attributed to the verified RemotePlay backend"}



def verify_focus_release(before, after, sender, backend_pid):
    require(type(backend_pid) is int and backend_pid > 0, "exact backend PID required")
    require(sender.get("cleanup_complete") is True, "owned sender cleanup required")
    require(sender.get("kind") == "controlled-synthetic-held-input-focus", "focus-mode receipt required")
    require(before.get("pid") == after.get("pid") == sender.get("fixture_pid"), "fixture process changed")
    require(before["A"]["id"] == after["A"]["id"] == sender.get("fixture_window"), "window changed")
    require(before["A"]["title"] == after["A"]["title"]
            == "RemotePlay scoped-window acceptance " + sender["fixture_nonce"], "fixture nonce changed")
    require(before["A"]["held_keys"] == [0] and before["A"]["held_buttons"] == [0], "native held baseline absent")
    require(before["A"].get("key_window") is True and after["B"].get("key_window") is True, "own A to B focus transition absent")
    require(all(not after[label][key] for label in ("A","B") for key in ("held_keys","held_buttons")), "native presses remain held")
    prefix=before.get("input_events",[]);events=after.get("input_events",[])
    require(events[:len(prefix)] == prefix, "native event prefix missing")
    own=[e for e in events[len(prefix):] if e.get("source_pid") == backend_pid]
    require(any(e["kind"] == "key_up" and e["key_code"] == 0 for e in own)
            and any(e["kind"] == "mouse_up" for e in own), "native release effects absent")
    require(not any(e["kind"] in ("mouse_down","key_down") and e["window"] == "B" for e in own), "input leaked into adjacent fixture")
    return {"kind":"native-focus-release-effects","verified":True,"backend_pid":backend_pid,
            "fixture_pid":after["pid"],"held_state_empty":True,"focus":"own A to own B"}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ("before","after","sender"):p.add_argument("--"+name,type=Path,required=True)
    p.add_argument("--backend-pid",type=int,required=True)
    p.add_argument("--mode",choices=("suite","focus-release"),default="suite");a=p.parse_args()
    try: print(json.dumps((verify if a.mode == "suite" else verify_focus_release)(*(json.loads(getattr(a,n).read_text()) for n in ("before","after","sender")),a.backend_pid)))
    except (ValueError,KeyError,TypeError,OSError):raise SystemExit("Native input effects not verified; no acceptance inferred")

if __name__ == "__main__":main()
