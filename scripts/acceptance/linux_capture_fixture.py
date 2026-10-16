#!/usr/bin/env python3
"""Own solid-color GTK fixture + existing product portal probe; no private pixels saved."""
import argparse
import json
import signal
import subprocess
import sys
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("source", choices=("window", "monitor"))
parser.add_argument("--trace-stages", action="store_true")
parser.add_argument("--selection-timeout-seconds", type=int, default=45, choices=range(5, 121), metavar="5..120")
args = parser.parse_args()

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, GLib, Gtk

root = Path(__file__).resolve().parents[2]
probe = root / "target/debug/examples/linux_portal_control"
if not probe.is_file():
    raise SystemExit("Build the existing linux_portal_control example in this checkout's target first")
display = Gdk.Display.get_default()
if display is None:
    raise SystemExit("No local desktop display; no capture started")
if args.source == "monitor" and display.get_n_monitors() != 1:
    raise SystemExit("This pure test fixture covers one monitor; no multi-monitor capture started")

window = Gtk.Window(title="Weavelo Capture Probe - pure test colors")
window.set_default_size(960, 540)
box = Gtk.EventBox()
box.set_name("weavelo-capture-fixture")
label = Gtk.Label(label="Weavelo capture test\nOnly solid colors and this label")
box.add(label)
window.add(box)
css = Gtk.CssProvider()
Gtk.StyleContext.add_provider_for_screen(display.get_default_screen(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
colors = ("#d82424", "#24c848", "#2864e0")
state = {"tick": 0, "child": None, "exit": 1}
def paint():
    state["tick"] += 1
    text = "#weavelo-capture-fixture { background-color: " + colors[state["tick"] % 3] + "; color: white; font-size: 28px; }"
    css.load_from_data(text.encode())
    box.queue_draw()
    return True
def close(*unused):
    child = state["child"]
    if child and child.poll() is None:
        child.send_signal(signal.SIGINT)
    Gtk.main_quit()
    return False
window.connect("delete-event", close)
paint()
window.show_all()
if args.source == "monitor":
    window.fullscreen_on_monitor(display.get_default_screen(), 0)
def start():
    if not window.get_mapped():
        print("FIXTURE_NOT_MAPPED capture_not_started=true", flush=True)
        Gtk.main_quit()
        return False
    print("FIXTURE " + json.dumps({"source": args.source, "title": window.get_title(), "gtk_backend": type(display).__name__, "width": window.get_allocated_width(), "height": window.get_allocated_height(), "test_content_only": True}), flush=True)
    if args.source == "monitor":
        geometry = display.get_monitor(0).get_geometry()
        if window.get_allocated_width() != geometry.width or window.get_allocated_height() != geometry.height:
            print("FULLSCREEN_FIXTURE_NOT_COVERING_MONITOR capture_not_started=true", flush=True)
            Gtk.main_quit()
            return False
    command = [str(probe), "--select-" + args.source, "--sample-frames=24", "--selection-timeout-seconds=" + str(args.selection_timeout_seconds)]
    if args.trace_stages:
        command.append("--trace-stages")
    print("NORMAL_PORTAL_SELECTION select_only_the_test_fixture=true", flush=True)
    state["child"] = subprocess.Popen(command, cwd=root, stdin=subprocess.DEVNULL)
    return False
def poll():
    child = state["child"]
    if child and child.poll() is not None:
        state["exit"] = child.returncode
        print("PROBE_EXIT " + str(child.returncode), flush=True)
        Gtk.main_quit()
        return False
    return True
def deadline():
    print("FIXTURE_DEADLINE_REACHED", flush=True)
    close()
    return False
GLib.timeout_add(450, paint)
GLib.timeout_add(1200, start)
GLib.timeout_add(100, poll)
GLib.timeout_add_seconds(args.selection_timeout_seconds + 15, deadline)
try:
    Gtk.main()
finally:
    child = state["child"]
    if child and child.poll() is None:
        child.send_signal(signal.SIGINT)
        try:
            child.wait(timeout=4)
        except subprocess.TimeoutExpired:
            child.terminate()
            try:
                child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=2)
    window.destroy()
    print("FIXTURE_CLOSED owned_probe_exited=" + str(child is None or child.poll() is not None).lower(), flush=True)
sys.exit(state["exit"])
