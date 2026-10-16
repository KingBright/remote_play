"""Explicit disposable installer scope; production paths/label are the default.

The fixture mode is bound to a fresh private temporary root and an independent
launchd label. It cannot point at the logged-in user's app, profile or service.
It preserves the same signing policy as production; there is no unsigned bypass.
"""
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import stat
import tempfile
from macos_release_guard import ReleaseRejected

MARKER = '.weavelo-isolated-fixture.json'
CURRENT = ContextVar('weavelo_installer_scope', default=None)


@dataclass(frozen=True)
class FixtureScope:
    root: Path
    label: str

    @property
    def home(self): return self.root / 'home'

    def validate_boundary(self):
        root = self.root
        if (not root.is_absolute() or root != root.resolve() or root == Path.home().resolve()
                or '..' in root.parts
                or not re.fullmatch(r'fun\.hackerlife\.weavelo\.lifecycle-test\.[0-9a-f]{32}', self.label)):
            raise ReleaseRejected('Invalid isolated fixture root or test service label')
        temp_roots = {Path('/tmp').resolve(), Path(tempfile.gettempdir()).resolve()}
        if not any(root.is_relative_to(base) and root != base for base in temp_roots):
            raise ReleaseRejected('Isolated installer fixture must stay under a temporary root')
        info = root.lstat()
        if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid()
                or stat.S_IMODE(info.st_mode) != 0o700):
            raise ReleaseRejected('Isolated fixture root is not private and owned')
        return self

    def validate(self):
        self.validate_boundary()
        root = self.root
        for path in [root, self.home, self.home / 'Applications']:
            info = path.lstat()
            if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid()
                    or stat.S_IMODE(info.st_mode) != 0o700):
                raise ReleaseRejected('Isolated fixture directory is not private and owned')
        marker = root / MARKER
        info = marker.lstat()
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600
                or info.st_size > 4096):
            raise ReleaseRejected('Invalid isolated fixture scope marker')
        expected = {'schema': 'weavelo.isolated-installer.v1', 'root': str(root),
                    'uid': os.getuid(), 'label': self.label}
        if json.loads(marker.read_text()) != expected:
            raise ReleaseRejected('Isolated fixture scope marker changed')
        return self


def initialize(root: Path, label: str) -> FixtureScope:
    root = root.resolve(strict=True)
    scope = FixtureScope(root, label).validate_boundary()
    if list(root.iterdir()): raise ReleaseRejected('Initialize only a fresh empty fixture root')
    scope.home.mkdir(mode=0o700)
    (scope.home / 'Applications').mkdir(mode=0o700)
    fd = os.open(root / MARKER, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as out:
        json.dump({'schema': 'weavelo.isolated-installer.v1', 'root': str(root),
                   'uid': os.getuid(), 'label': label}, out)
        out.flush(); os.fsync(out.fileno())
    return scope.validate()


@contextmanager
def isolated(root: Path, label: str):
    scope = FixtureScope(root.resolve(strict=True), label).validate()
    token = CURRENT.set(scope)
    try: yield scope
    finally: CURRENT.reset(token)


def current():
    scope = CURRENT.get()
    if scope is not None: scope.validate()
    return scope
