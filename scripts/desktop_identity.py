"""Public desktop identity and collision checks. No profile/credential bodies are read."""
import json
import os
from pathlib import Path
import stat

ROOT = Path(__file__).resolve().parents[1]


def identity():
    data = json.loads((ROOT / 'deploy/desktop/product-identity.json').read_text())
    if (data['schema'] != 1 or data['product'] != 'Weavelo'
            or data['application_id'] != 'fun.hackerlife.weavelo.desktop'
            or data['service_label'] != data['application_id'] + '.host'
            or data['install_directory'] != 'Weavelo' or data['profile_directory'] != 'Desktop'
            or data['control_port'] in (0, 39271) or data['discovery_port'] in (0, 38117)):
        raise ValueError('Unexpected desktop identity or retained namespace collision')
    return data


def paths(home):
    home = Path(home).absolute()
    if '..' in home.parts or home != home.resolve():
        raise ValueError('Canonical user home required')
    data = identity()
    app = home / 'Applications' / data['install_directory'] / 'Weavelo.app'
    profile = home / 'Library/Application Support/Weavelo' / data['profile_directory']
    return {'app': app, 'profile': profile,
            'plist': home / 'Library/LaunchAgents' / (data['service_label'] + '.plist'),
            'state': home / 'Library/Application Support/Weavelo/DesktopInstall',
            'gui_socket': profile / 'gui-state/gui-receive.sock',
            'background_socket': profile / 'background.sock'}


def check_collision(home):
    selected = paths(home)
    # Reject redirected ancestors without reading any retained profile or following links.
    for path in selected.values():
        for parent in (path, *path.parents):
            if parent == Path(home).parent: break
            try: info = parent.lstat()
            except FileNotFoundError: continue
            if stat.S_ISLNK(info.st_mode):
                raise ValueError('Desktop destination has a symlink ancestor')
            if parent != path and not stat.S_ISDIR(info.st_mode):
                raise ValueError('Desktop destination ancestor is not a directory')
            if info.st_uid != os.getuid() or info.st_mode & 0o022:
                raise ValueError('Desktop destination must be user owned and not group/world writable')
    for key in ('app', 'plist', 'state'):
        if selected[key].exists():
            raise ValueError('Independent destination already exists; inspect its original installation, do not replay')
    if selected['profile'].exists():
        raise ValueError('Independent profile already exists; explicit adoption is required')
    return selected


def launch_agent(home):
    data = identity(); selected = paths(home)
    return {'Label': data['service_label'],
            'ProgramArguments': [str(selected['app'] / 'Contents/MacOS/weavelo'), '--background-service'],
            'RunAtLoad': True, 'KeepAlive': {'SuccessfulExit': False},
            'EnvironmentVariables': {'WEAVELO_DEVICE_GROUP_DIR': str(selected['profile']),
                'WEAVELO_HOST_BIND_ADDR': f"0.0.0.0:{data['control_port']}",
                'WEAVELO_DISCOVERY_PORT': str(data['discovery_port']),
                'WEAVELO_RELAY': '0', 'WEAVELO_P2P': '0', 'WEAVELO_HEADLESS': '1'}}
