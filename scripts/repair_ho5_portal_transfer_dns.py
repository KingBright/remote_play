#!/usr/bin/env python3
"""One reviewed, user-operated HO5 DNS change. No privilege acquisition.

Default: inspect the exact change only. --apply must be invoked by the user in
HO5's native administrator terminal. Configuration contents never leave this
process or appear in diagnostics. This is not a Remote Hosts admin helper.
"""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import socket
import stat
import subprocess
import tempfile


CONFIG = Path("/etc/sing-box/config.json")
BACKUP = CONFIG.with_name("config.json.rp-exact-dns-20261010.backup")
EXPECTED_SHA256 = "4217b7c50bc8032d5f5f854a2e73edc0167851cd3dd013f6b9e3953634337435"
DOMAIN = "sdmntprcentralus.oaiusercontent.com"
SERVICE = "sing-box.service"
RULE = {"domain": [DOMAIN], "action": "route", "server": "dns-remote"}
MAX_BYTES = 1024 * 1024


class RepairFailure(RuntimeError):
    def __init__(self, code, rolled_back=False):
        self.code = code
        self.rolled_back = rolled_back
        super().__init__(code)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def unique_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON field; semantic preservation cannot be proven")
        result[key] = value
    return result


def patch_document(document):
    """Return a copy with exactly one rule prepended; all other fields survive."""
    if not isinstance(document, dict) or not isinstance(document.get("dns"), dict):
        raise ValueError("dns object missing")
    dns = document["dns"]
    servers, rules = dns.get("servers"), dns.get("rules")
    if not isinstance(servers, list) or not isinstance(rules, list):
        raise ValueError("dns servers/rules missing")
    matching = [s for s in servers if isinstance(s, dict) and s.get("tag") == "dns-remote"]
    if len(matching) != 1 or matching[0].get("type") != "tls" or matching[0].get("detour") != "proxy":
        raise ValueError("expected existing TLS dns-remote through proxy missing")
    for rule in rules:
        if not isinstance(rule, dict):
            raise ValueError("unsupported existing DNS rule shape")
        domains = rule.get("domain", [])
        if not isinstance(domains, list) or DOMAIN in domains:
            raise ValueError("exact-domain rule already exists or has unsupported shape")
    result = dict(document)
    result["dns"] = dict(dns)
    result["dns"]["rules"] = [dict(RULE), *rules]
    # No normalization of rule contents, outbound definitions, DNS server
    # configuration, subscription credentials, or routing rules.
    return result


def read_regular(path):
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > MAX_BYTES:
            raise ValueError("configuration must be one bounded regular file")
        with os.fdopen(fd, "rb", closefd=False) as source:
            data = source.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("configuration exceeds bounded size")
        return data, info
    finally:
        os.close(fd)


def check_secure_parent(path):
    for directory in (path.parent, path.parent.parent):
        info = directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or stat.S_IMODE(info.st_mode) & 0o022:
            raise ValueError("administrator configuration directory is not protected")


def command_ok(args, timeout=30):
    # stdout/stderr may include configuration values on parser errors. Retain
    # neither in the receipt, including when the subprocess exits nonzero.
    result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            timeout=timeout, check=False)
    if result.returncode:
        raise RuntimeError("validated command failed (details suppressed)")


def service_executable():
    result = subprocess.run(["/usr/bin/systemctl", "show", SERVICE, "--property=ExecStart", "--value"],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=True)
    text = result.stdout.decode("utf-8", errors="strict")
    match = re.fullmatch(r"\s*\{ path=([^ ;]+) ; argv\[\]=(.*?) ; ignore_errors=.*\}\s*", text, re.DOTALL)
    if not match or text.count("{ path=") != 1:
        raise ValueError("one supported ExecStart required")
    executable = Path(match[1]).resolve(strict=True)
    argv = shlex.split(match[2])
    uses_config = any(a == "-c" and b == str(CONFIG) or a == "-C" and b == str(CONFIG.parent)
                      for a, b in zip(argv, argv[1:]))
    info = executable.stat()
    if executable.name != "sing-box" or info.st_uid != 0 or stat.S_IMODE(info.st_mode) & 0o022 or not uses_config:
        raise ValueError("service executable/configuration binding does not match")
    return str(executable)


def write_candidate(data, info):
    fd, name = tempfile.mkstemp(prefix=".rp-exact-dns-", suffix=".json", dir=CONFIG.parent)
    path = Path(name)
    try:
        os.fchmod(fd, stat.S_IMODE(info.st_mode))
        os.fchown(fd, info.st_uid, info.st_gid)
        with os.fdopen(fd, "wb", closefd=False) as stream:
            stream.write(data)
            stream.flush()
            os.fsync(fd)
        # Preserve Linux labels and other existing xattrs without displaying
        # their values. A failure leaves the live configuration untouched.
        for attribute in os.listxattr(CONFIG, follow_symlinks=False):
            os.setxattr(path, attribute, os.getxattr(CONFIG, attribute, follow_symlinks=False),
                        follow_symlinks=False)
        return path
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    finally:
        os.close(fd)


def fsync_directory():
    fd = os.open(CONFIG.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def create_backup(original):
    fd = os.open(BACKUP, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "wb", closefd=False) as stream:
            stream.write(original)
            stream.flush()
            os.fsync(fd)
        fsync_directory()
    finally:
        os.close(fd)


def verify_resolver():
    try:
        addresses = sorted({entry[4][0] for entry in socket.getaddrinfo(DOMAIN, 443, type=socket.SOCK_STREAM)})
        public = bool(addresses) and all(ipaddress.ip_address(a).is_global for a in addresses)
        return {"addresses": addresses, "all_public": public,
                "source_https_or_transfer_test": "not_run"}
    except OSError:
        return {"addresses": [], "all_public": False, "source_https_or_transfer_test": "not_run"}


def execute(apply=False):
    original, info = read_regular(CONFIG)
    if sha256(original) != EXPECTED_SHA256:
        raise ValueError("configuration hash changed; do not apply this reviewed script")
    document = json.loads(original, object_pairs_hook=unique_pairs)
    proposed = patch_document(document)
    candidate_data = (json.dumps(proposed, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    receipt = {"action": "prepend_exact_domain_dns_rule", "domain": DOMAIN, "server": "dns-remote",
               "old_sha256": EXPECTED_SHA256, "new_sha256": sha256(candidate_data),
               "semantic_changes": [{"path": "/dns/rules/0", "insert": RULE}],
               "system_changes_confirmed": False, "administrator_steps_executed": False}
    if not apply:
        receipt["state"] = "preflight_only"
        return receipt
    if os.geteuid() != 0:
        raise PermissionError("--apply requires the user's native administrator terminal")
    check_secure_parent(CONFIG)
    if info.st_uid != 0 or stat.S_IMODE(info.st_mode) & 0o022:
        raise ValueError("live configuration must be administrator-owned and protected")
    executable = service_executable()
    command_ok(["/usr/bin/systemctl", "is-active", "--quiet", SERVICE], 10)
    if BACKUP.exists() or BACKUP.is_symlink():
        raise ValueError("reviewed backup path already exists; never overwrite it")
    candidate = write_candidate(candidate_data, info)
    changed = False
    try:
        command_ok([executable, "check", "-c", str(candidate)])
        current, current_info = read_regular(CONFIG)
        if sha256(current) != EXPECTED_SHA256 or (current_info.st_dev, current_info.st_ino) != (info.st_dev, info.st_ino):
            raise ValueError("configuration changed during preflight")
        create_backup(original)
        os.replace(candidate, CONFIG)
        changed = True
        fsync_directory()
        command_ok(["/usr/bin/systemctl", "restart", SERVICE])
        command_ok(["/usr/bin/systemctl", "is-active", "--quiet", SERVICE], 10)
        actual, _ = read_regular(CONFIG)
        if sha256(actual) != sha256(candidate_data):
            raise ValueError("live configuration changed after restart")
        receipt.update(state="applied", system_changes_confirmed=True,
                       administrator_steps_executed=True, backup=str(BACKUP), resolver=verify_resolver())
        return receipt
    except BaseException:
        if changed:
            actual, _ = read_regular(CONFIG)
            if sha256(actual) != sha256(candidate_data):
                raise RepairFailure("rollback_conflict_current_configuration_changed") from None
            backup, _ = read_regular(BACKUP)
            if sha256(backup) != EXPECTED_SHA256:
                raise RepairFailure("rollback_backup_hash_mismatch") from None
            rollback = write_candidate(backup, info)
            os.replace(rollback, CONFIG)
            fsync_directory()
            try:
                command_ok(["/usr/bin/systemctl", "restart", SERVICE])
                command_ok(["/usr/bin/systemctl", "is-active", "--quiet", SERVICE], 10)
                restored, _ = read_regular(CONFIG)
                if sha256(restored) != EXPECTED_SHA256:
                    raise ValueError("rollback configuration changed during restart")
            except BaseException:
                raise RepairFailure("original_restored_service_recovery_unconfirmed", True) from None
            raise RepairFailure("original_restored_service_active", True) from None
        raise
    finally:
        candidate.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    try:
        print(json.dumps(execute(args.apply), ensure_ascii=False, sort_keys=True))
        return 0
    except Exception as error:
        # Do not expose a JSON decode excerpt, filesystem contents, service
        # argv, subprocess output, or arbitrary exception text.
        receipt = {"state": "failed", "error_type": type(error).__name__,
                   "system_changes_confirmed": None if args.apply else False, "details_suppressed": True}
        if isinstance(error, RepairFailure):
            receipt.update(error_code=error.code, rollback_original_restored=error.rolled_back,
                           administrator_steps_executed=True)
        print(json.dumps(receipt))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
