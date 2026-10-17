#!/usr/bin/env python3
"""Bounded GTK window input effects; never hooks global input or stores key data.

Only progress through an explicitly public numeric challenge is retained. GTK
cannot attribute compositor events to a sender PID: product attribution needs
separate verified source/session/installed-image evidence. Offscreen self-tests
exercise toolkit dispatch only and can never pass the live-effects validator.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import time

TITLE = 'Weavelo synthetic input '


def require(ok, reason):
    if not ok:
        raise ValueError(reason)


def identity():
    # Parenthesized comm may contain whitespace and ')'. starttime is field 22.
    raw = Path('/proc/self/stat').read_text()
    return {'pid': os.getpid(), 'start_ticks': int(raw.rsplit(')', 1)[1].split()[19])}


class Effects:
    def __init__(self, nonce, challenge, process, mode='native-window'):
        require(re.fullmatch(r'[0-9a-f]{32}', nonce) is not None, '32 lowercase hex nonce required')
        require(re.fullmatch(r'[0-9]{8}', challenge) is not None, 'Eight public test digits required')
        require(mode in ('native-window', 'gtk-offscreen-selftest'), 'Unknown observation mode')
        self.nonce, self.challenge, self.process, self.mode = nonce, challenge, process, mode
        self.focused = False
        self.focus_gains = self.focus_losses = self.clicks = self.matched = 0
        self.key_pending = self.button_pending = False
        self.coverage_lost = False
        self.started_ns = time.monotonic_ns()

    def focus(self, active):
        if active != self.focused:
            if active:
                self.focus_gains += 1
            else:
                self.focus_losses += 1
                # Do not fabricate key/button releases on loss of observation.
                if self.key_pending or self.button_pending:
                    self.coverage_lost = True
            self.focused = active
            return True
        return False

    def button(self, pressed, primary, within):
        if not self.focused or not primary or not within:
            return False
        if pressed:
            if self.button_pending:
                self.coverage_lost = True
            self.button_pending = True
            return True
        elif self.button_pending:
            self.button_pending = False
            self.clicks = min(self.clicks + 1, 100)
            return True
        return False

    def key(self, pressed, keyval, blocked_modifiers=False):
        # No arbitrary key values, characters, timestamps, or event logs retained.
        if not self.focused or blocked_modifiers or self.matched == len(self.challenge):
            return False
        if keyval != ord(self.challenge[self.matched]):
            return False
        if pressed:
            changed = not self.key_pending
            self.key_pending = True  # autorepeat cannot count without a release
            return changed
        elif self.key_pending:
            self.key_pending = False
            self.matched += 1
            return True
        return False

    def snapshot(self, backend, closed=False):
        return {'schema': 1, 'kind': 'linux-synthetic-window-effects',
            'mode': self.mode, **self.process, 'nonce': self.nonce,
            'title': TITLE + self.nonce, 'challenge': self.challenge,
            'backend': backend, 'started_ns': self.started_ns,
            'observed_ns': time.monotonic_ns(), 'closed': closed,
            'focused': self.focused, 'focus_gains': self.focus_gains,
            'focus_losses': self.focus_losses, 'clicks': self.clicks,
            'matched_digits': self.matched,
            'synthetic_text': self.challenge[:self.matched],
            'expected_key_pending': self.key_pending,
            'primary_button_pending': self.button_pending,
            'coverage_lost': self.coverage_lost,
            'raw_keyboard_recorded': False, 'global_input_hook': False,
            'sender_pid_attribution_available': False}


class Evidence:
    """One new private directory; never overwrite unknown or changed evidence."""
    def __init__(self, root):
        root = root.absolute()
        require('..' not in root.parts, 'Canonical output path required')
        for p in [root, *root.parents]:
            require(not p.is_symlink(), 'Linked output path refused')
        root.mkdir(mode=0o700)  # Existing evidence is always preserved.
        self.root = root
        self.fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.inode = os.fstat(self.fd).st_ino
        self.previous = None

    def write(self, state):
        require(self.fd is not None and self.root.lstat().st_ino == self.inode,
                'Output directory replaced')
        if self.previous is not None:
            fd = os.open('state.json', os.O_RDONLY | os.O_NOFOLLOW, dir_fd=self.fd)
            with os.fdopen(fd, 'rb') as f:
                m = os.fstat(f.fileno())
                require(stat.S_ISREG(m.st_mode) and m.st_nlink == 1 and m.st_uid == os.getuid(),
                        'Unknown output file')
                require(hashlib.sha256(f.read(32769)).hexdigest() == self.previous,
                        'Evidence changed externally; preserve it')
        else:
            try:
                os.stat('state.json', dir_fd=self.fd, follow_symlinks=False)
            except FileNotFoundError:
                pass
            else:
                raise ValueError('Unknown state file preserved')
        raw = (json.dumps(state, sort_keys=True) + '\n').encode()
        require(len(raw) <= 32768, 'Bounded evidence required')
        fd = os.open('.state.pending', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                     0o600, dir_fd=self.fd)
        with os.fdopen(fd, 'wb') as f:
            f.write(raw)
            f.flush()
            os.fsync(f.fileno())
        os.replace('.state.pending', 'state.json', src_dir_fd=self.fd, dst_dir_fd=self.fd)
        self.previous = hashlib.sha256(raw).hexdigest()

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


def verify(before, after):
    for state in (before, after):
        require(state['schema'] == 1 and state['kind'] == 'linux-synthetic-window-effects',
                'Wrong evidence schema')
        require(state['mode'] == 'native-window', 'Self-test is not live input evidence')
        require(state['raw_keyboard_recorded'] is False and state['global_input_hook'] is False,
                'Wrong observation boundary')
        require(state['coverage_lost'] is False, 'Input observation coverage lost')
        for name in ('pid', 'start_ticks', 'started_ns', 'observed_ns', 'clicks',
                     'matched_digits', 'focus_gains', 'focus_losses'):
            require(type(state[name]) is int and state[name] >= 0, 'Invalid integer evidence')
        for name in ('focused', 'closed', 'expected_key_pending', 'primary_button_pending'):
            require(type(state[name]) is bool, 'Invalid boolean evidence')
        require(0 <= state['matched_digits'] <= 8 and state['clicks'] <= 100,
                'Invalid bounded counters')
        require(state['title'] == TITLE + state['nonce'], 'Window title drift')
        require(state['synthetic_text'] == state['challenge'][:state['matched_digits']],
                'Challenge progress inconsistent')
    for name in ('pid', 'start_ticks', 'nonce', 'challenge', 'title', 'started_ns', 'backend'):
        require(before[name] == after[name], 'Fixture identity changed')
    Effects(before['nonce'], before['challenge'], {})  # Validate public challenge format.
    require(type(before['pid']) is int and before['pid'] > 0 and before['start_ticks'] > 0,
            'Native process identity absent')
    require(before['matched_digits'] == 0 and before['clicks'] == 0 and not before['closed'],
            'Clean baseline required before sending input')
    require(not before['expected_key_pending'] and not before['primary_button_pending'],
            'Baseline already has held input')
    require(after['focus_gains'] >= before['focus_gains'] and after['focus_losses'] >= before['focus_losses'],
            'Focus counters reversed')
    require(0 < after['observed_ns'] - before['observed_ns'] <= 180_000_000_000,
            'Stale, reversed or unbounded evidence')
    require(after['clicks'] == 1 and after['matched_digits'] == 8,
            'One native click and all test digits required')
    require(after['focus_gains'] > 0 and after['focused'], 'Fixture focus not observed')
    require(not after['expected_key_pending'] and not after['primary_button_pending'],
            'Expected synthetic input remains held')
    return {'kind': 'linux-synthetic-window-effects-validation',
        'native_window_effects_verified': True, 'pid': after['pid'],
        'nonce': after['nonce'], 'clicks': 1, 'synthetic_text': after['challenge'],
        'product_sender_attribution_verified': False, 'end_to_end_acceptance': False,
        'boundary': 'Combine with independent installed-image, source/session and presented-frame evidence; GTK provides no sender PID.'}


def gtk_fixture(model, offscreen=False, changed=lambda: None):
    import gi
    gi.require_version('Gtk', '3.0')
    gi.require_version('Gdk', '3.0')
    from gi.repository import Gdk, Gtk
    require(Gtk.init_check()[0], 'GTK display unavailable; no desktop changes attempted')
    window = Gtk.OffscreenWindow() if offscreen else Gtk.Window(title=TITLE + model.nonce)
    if not offscreen:
        # Do not steal focus or activate over existing work; the test click focuses it.
        window.set_focus_on_map(False)
    window.set_default_size(640, 360)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
    window.add(box)
    info = Gtk.Label(label='Synthetic test only: click the blue area once, then send ' + model.challenge)
    box.pack_start(info, False, False, 12)
    area = Gtk.EventBox()
    area.set_can_focus(True)
    css = Gtk.CssProvider()
    css.load_from_data(b'* { background-color: #1040b0; }')
    area.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    box.pack_start(area, True, True, 0)
    area.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK |
        Gdk.EventMask.KEY_PRESS_MASK | Gdk.EventMask.KEY_RELEASE_MASK | Gdk.EventMask.FOCUS_CHANGE_MASK)

    def save():
        info.set_text('Synthetic test ' + model.challenge + '\nClicks: ' + str(model.clicks) +
                      '  matched digits: ' + str(model.matched) + '/8')
        changed()

    def accepted(event):
        return bool(event.send_event) == offscreen

    def button(widget, event):
        if accepted(event):
            if event.type == Gdk.EventType.BUTTON_PRESS and event.button == 1:
                widget.grab_focus()  # Only after a click inside this owned widget.
            if model.button(event.type == Gdk.EventType.BUTTON_PRESS, event.button == 1,
                            0 <= event.x < widget.get_allocated_width() and 0 <= event.y < widget.get_allocated_height()):
                save()
        return True

    def key(widget, event):
        if accepted(event):
            blocked = bool(event.state & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.MOD1_MASK |
                Gdk.ModifierType.SUPER_MASK | Gdk.ModifierType.SHIFT_MASK))
            if model.key(event.type == Gdk.EventType.KEY_PRESS, event.keyval, blocked):
                save()
        return True  # Never pass arbitrary typing into an entry, clipboard or log.

    def focus(widget, event):
        # GTK may synthesize widget focus notifications from native toplevel
        # focus changes. This is still confined to our widget, never a global hook.
        if model.focus(bool(event.in_)):
            save()
        return False

    def active_changed(*unused):
        if not window.is_active():
            if model.focus(False):
                save()

    area.connect('button-press-event', button)
    area.connect('button-release-event', button)
    area.connect('key-press-event', key)
    area.connect('key-release-event', key)
    area.connect('focus-in-event', focus)
    area.connect('focus-out-event', focus)
    if not offscreen:
        window.connect('notify::is-active', active_changed)
    return Gtk, Gdk, window, area


def self_test():
    model = Effects('a' * 32, '73122905', identity(), 'gtk-offscreen-selftest')
    Gtk, Gdk, window, area = gtk_fixture(model, offscreen=True)
    try:
        window.show_all()
        while Gtk.events_pending():
            Gtk.main_iteration_do(False)
        require(window.get_window().get_window_type() == Gdk.WindowType.OFFSCREEN,
                'Self-test must never map a desktop toplevel')
        def send(kind, **values):
            event = Gdk.Event.new(kind)
            event.window = area.get_window()
            event.send_event = True
            for k, v in values.items():
                setattr(event, k, v)
            area.event(event)  # Actual GObject/GDK -> GTK signal dispatch, no OS injection.
        send(Gdk.EventType.FOCUS_CHANGE, in_=True)
        send(Gdk.EventType.KEY_PRESS, keyval=ord('z'), state=Gdk.ModifierType(0))
        send(Gdk.EventType.KEY_RELEASE, keyval=ord('z'), state=Gdk.ModifierType(0))
        require(model.matched == 0, 'Unexpected typing was accepted')
        send(Gdk.EventType.BUTTON_PRESS, button=1, x=20., y=20.)
        send(Gdk.EventType.BUTTON_RELEASE, button=1, x=20., y=20.)
        for digit in model.challenge:
            send(Gdk.EventType.KEY_PRESS, keyval=ord(digit), state=Gdk.ModifierType(0))
            send(Gdk.EventType.KEY_RELEASE, keyval=ord(digit), state=Gdk.ModifierType(0))
        require(model.clicks == 1 and model.matched == 8 and not model.key_pending,
                'Native GTK synthetic dispatch did not reach observer')
        send(Gdk.EventType.FOCUS_CHANGE, in_=False)
        require(model.focus_losses == 1 and not model.focused, 'Focus dispatch missing')
        return {'kind': 'linux-input-observer-native-gtk-selftest', 'passed': True,
            'offscreen_only': True, 'desktop_input_injected': False,
            'native_gtk_dispatch_verified': True, 'clicks': model.clicks,
            'matched_digits': model.matched, 'focus_loss_observed': True,
            'end_to_end_acceptance': False, 'mode': model.mode}
    finally:
        window.destroy()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest='command', required=True)
    sub.add_parser('self-test')
    run = sub.add_parser('observe')
    run.add_argument('--output-dir', type=Path, required=True)
    run.add_argument('--nonce', required=True)
    run.add_argument('--challenge', required=True)
    run.add_argument('--seconds', type=int, choices=range(5, 181), default=90)
    check = sub.add_parser('verify')
    check.add_argument('--before', type=Path, required=True)
    check.add_argument('--after', type=Path, required=True)
    a = p.parse_args()
    if a.command == 'self-test':
        result = self_test()
    elif a.command == 'verify':
        def read(path):
            require(path.stat().st_size <= 32768, 'Bounded evidence required')
            return json.loads(path.read_text())
        result = verify(read(a.before), read(a.after))
    else:
        from gi.repository import GLib
        model = Effects(a.nonce, a.challenge, identity())
        output = Evidence(a.output_dir)
        Gtk = window = None
        try:
            backend = 'unmapped'
            def save():
                output.write(model.snapshot(backend))
            Gtk, Gdk, window, area = gtk_fixture(model, changed=save)
            backend = type(Gdk.Display.get_default()).__name__
            window.connect('delete-event', lambda *unused: (Gtk.main_quit(), False)[1])
            window.show_all()
            save()
            GLib.timeout_add_seconds(a.seconds, lambda: (Gtk.main_quit(), False)[1])
            Gtk.main()
            output.write(model.snapshot(backend, closed=True))
            result = {'kind': 'linux-input-observer-closed', 'state': str(output.root/'state.json'),
                      'end_to_end_acceptance': False}
        finally:
            if window is not None:
                window.destroy()
            output.close()
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
