#!/usr/bin/env python3
"""Fail closed on debug, stale JNI, mismatched ABI/version, and invalid native alignment."""
from __future__ import annotations
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import zipfile


def run(args: list[str]) -> str:
    result = subprocess.run(args, capture_output=True, text=True, timeout=90, check=True)
    return result.stdout + result.stderr


def exported_symbols(data: bytes) -> set[str]:
    """Read the ELF64 dynamic symbol table, not unrelated strings in executable data."""
    shoff = struct.unpack_from('<Q', data, 40)[0]
    entry_size, count = struct.unpack_from('<HH', data, 58)
    sections = [struct.unpack_from('<IIQQQQIIQQ', data, shoff + i * entry_size) for i in range(count)]
    names = set()
    for section in sections:
        if section[1] != 11:  # SHT_DYNSYM
            continue
        strings = sections[section[6]]
        table = data[strings[4]:strings[4] + strings[5]]
        stride = section[9] or 24
        for offset in range(section[4], section[4] + section[5], stride):
            name, info, other, index, _, _ = struct.unpack_from('<IBBHQQ', data, offset)
            if index == 0 or info >> 4 not in (1, 2) or other & 3 not in (0, 3):
                continue
            end = table.find(b'\0', name)
            if end >= 0:
                names.add(table[name:end].decode('utf-8', errors='strict'))
    return names


def compiled_manifest_attributes(tree: str) -> dict:
    """Inspect compiled manifest fields without resolving unrelated icon resources."""
    stack = []
    sections = {"manifest": {}, "application": {}, "uses-sdk": {}}
    counts = {name: 0 for name in sections}
    for line in tree.splitlines():
        element = re.match(r"^(\s*)E: ([\w.-]+)(?:\s|$)", line)
        if element:
            depth = len(element[1])
            while stack and stack[-1][0] >= depth:
                stack.pop()
            name = element[2]
            stack.append((depth, name))
            if name in sections:
                valid = (name == "manifest" and len(stack) == 1) or (
                    name != "manifest" and len(stack) == 2 and stack[0][1] == "manifest"
                )
                if valid:
                    counts[name] += 1
            continue
        attribute = re.match(r"^\s*A: ([^\s=(]+)(?:\([^)]*\))?=(.*)$", line)
        if attribute and stack:
            name = stack[-1][1]
            valid = (name == "manifest" and len(stack) == 1) or (
                name in ("application", "uses-sdk") and len(stack) == 2 and stack[0][1] == "manifest"
            )
            if valid:
                if attribute[1] in sections[name]:
                    raise ValueError(f"Duplicate compiled manifest attribute: {attribute[1]}")
                sections[name][attribute[1]] = attribute[2]
    if any(count != 1 for count in counts.values()):
        raise ValueError(f"Missing or ambiguous compiled manifest sections: {counts}")

    def string(section: str, name: str) -> str:
        raw = sections[section].get(name, "")
        value = re.match(r'^"([^"\\]*)"(?:\s|$)', raw)
        if not value:
            raise ValueError(f"Expected literal compiled manifest string: {name}")
        return value[1]

    def integer(section: str, name: str) -> int:
        raw = sections[section].get(name, "")
        value = re.fullmatch(r"\(type 0x1[01]\)(0x[0-9a-fA-F]+)", raw.strip())
        if not value:
            raise ValueError(f"Expected literal compiled manifest integer: {name}")
        return int(value[1], 16)

    debug = sections["application"].get("android:debuggable")
    if debug is not None:
        value = re.fullmatch(r"\(type 0x12\)(0x[0-9a-fA-F]+)", debug.strip())
        if not value:
            raise ValueError("Unresolved or invalid debuggable attribute; refusing publication")
        if int(value[1], 16) != 0:
            raise ValueError("Refusing to publish a debuggable APK")
    return dict(package=string("manifest", "package"),
                version=string("manifest", "android:versionName"),
                version_code=integer("manifest", "android:versionCode"),
                min_sdk=integer("uses-sdk", "android:minSdkVersion"),
                target_sdk=integer("uses-sdk", "android:targetSdkVersion"), debuggable=False)


def verify(apk: Path, root: Path) -> dict:
    sdk = Path(os.environ.get('ANDROID_HOME') or os.environ.get('ANDROID_SDK_ROOT') or Path.home() / 'Library/Android/sdk')
    tools = sorted((sdk / 'build-tools').glob('*'), key=lambda p: tuple(int(n) for n in re.findall(r'\d+', p.name)))
    if not tools:
        raise ValueError('Android SDK build tools not found')
    tools = tools[-1]
    manifest = compiled_manifest_attributes(run([
        str(tools / 'aapt'), 'dump', 'xmltree', str(apk), 'AndroidManifest.xml'
    ]))
    if manifest['package'] != 'fun.hackerlife.weavelo':
        raise ValueError('Unexpected application identity')
    gradle = (root / 'android/app/build.gradle.kts').read_text()
    expected_code = re.search(r'versionCode\s*=\s*(\d+)', gradle)[1]
    expected_name = re.search(r'versionName\s*=\s*"([^"]+)"', gradle)[1]
    if (str(manifest['version_code']), manifest['version']) != (expected_code, expected_name):
        raise ValueError('APK version does not match the source release')
    certificate = run([str(tools / 'apksigner'), 'verify', '--verbose', '--print-certs', str(apk)])
    if 'Android Debug' in certificate:
        raise ValueError('Refusing a debug signing certificate')
    fingerprint = re.search(r'Signer #1 certificate SHA-256 digest: ([0-9a-f]+)', certificate)
    if not fingerprint:
        raise ValueError('No verified release signer fingerprint')
    if fingerprint[1] != '215aedabf17f40a454ad381d9027772eef9446346cae3db031cac2a5ae51c3a5':
        raise ValueError('Release signer differs from the previously published app')
    run([str(tools / 'zipalign'), '-c', '-P', '16', '4', str(apk)])
    expected_jni = set()
    for source in (root / 'android/app/src/main/java/fun/hackerlife/weavelo').glob('*.kt'):
        for method in re.findall(r'external\s+fun\s+(\w+)', source.read_text()):
            expected_jni.add(f'Java_fun_hackerlife_weavelo_{source.stem}_{method}')
    if not expected_jni:
        raise ValueError("Weavelo JNI declarations are missing; cannot verify native linkage")
    libraries = {}
    with zipfile.ZipFile(apk) as archive:
        abis = set()
        for name in archive.namelist():
            if not name.startswith('lib/') or not name.endswith('.so'):
                continue
            abi = name.split('/')[1]; abis.add(abi)
            data = archive.read(name)
            if data[:6] != b'\x7fELF\x02\x01':
                raise ValueError(f'Expected a 64-bit little-endian ELF: {name}')
            machine = struct.unpack_from('<H', data, 18)[0]
            if (abi, machine) not in [('arm64-v8a', 183), ('x86_64', 62)]:
                raise ValueError(f'ELF/ABI mismatch: {name}')
            offset = struct.unpack_from('<Q', data, 32)[0]
            size, count = struct.unpack_from('<HH', data, 54)
            loads = []
            for i in range(count):
                entry = offset + i * size
                if struct.unpack_from('<I', data, entry)[0] == 1:
                    alignment = struct.unpack_from('<Q', data, entry + 48)[0]
                    loads.append(alignment)
            if not loads or min(loads) < 16384:
                raise ValueError(f'Native library is not 16 KB aligned: {name}, {loads}')
            libraries[name] = hashlib.sha256(data).hexdigest()
        if abis != {'arm64-v8a'}:
            raise ValueError(f'Release ABI set must be exactly the supported ARM64 target: {abis}')
        for abi in abis:
            name = f'lib/{abi}/libweavelo_android.so'
            data = archive.read(name)
            missing = sorted(expected_jni - exported_symbols(data))
            if missing:
                raise ValueError(f'Missing current JNI entry points in {abi}: {missing}')
            staged = root / 'android/app/src/main/jniLibs' / abi / 'libweavelo_android.so'
            if hashlib.sha256(staged.read_bytes()).hexdigest() != libraries[name]:
                raise ValueError('APK contains stale Weavelo native code')
    return dict(version=manifest['version'], version_code=manifest['version_code'],
                min_sdk=manifest['min_sdk'], target_sdk=manifest['target_sdk'], debuggable=False,
                signer_sha256=fingerprint[1], abi=sorted(abis), native_16kb_aligned=True,
                jni_entry_points=len(expected_jni), libraries=libraries,
                size_bytes=apk.stat().st_size, sha256=hashlib.sha256(apk.read_bytes()).hexdigest())


if __name__ == '__main__':
    try:
        root = Path(__file__).resolve().parent.parent
        apk = Path(sys.argv[1]).resolve()
        result = verify(apk, root)
        apk.with_suffix('.verification.json').write_text(json.dumps(result, indent=2) + '\n')
        print('RELEASE_APK_VERIFIED', json.dumps(result), flush=True)
    except Exception as error:
        print(f'RELEASE_APK_REJECTED: {error}', file=sys.stderr)
        sys.exit(1)
