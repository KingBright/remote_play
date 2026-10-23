"""Linux release activity policy and metadata-only collector.

No /proc reads, lsof, command arguments, privilege changes, or receipt import.
Missing reference providers are unknown, never evidence of an idle release.
"""
import datetime
import os
from pathlib import PurePosixPath
import re
import subprocess

SCOPES = ("command", "executable", "cwd", "files", "maps")
LAUNCH_SCOPES = ("systemd-user", "systemd-system", "xdg-autostart")
COMPILERS = {"cargo", "rustc", "rustdoc", "clippy-driver"}
PS_COMMAND = ("/bin/ps", "-eo", "pid=,ppid=,uid=,lstart=,comm=")
MAX_BYTES = 2 * 1024 * 1024
MAX_PROCESSES = 100000


class ObservationError(RuntimeError):
    pass


def natural(value, *, zero=False):
    if type(value) is not int or value < (0 if zero else 1):
        raise ObservationError("invalid numeric observation")
    return value


def canonical_path(value):
    if (type(value) is not str or not value.startswith("/") or
            any(ord(c) < 32 for c in value) or
            str(PurePosixPath(value)) != value or ".." in PurePosixPath(value).parts):
        raise ObservationError("noncanonical observed path")
    return value


def parse_processes(text):
    """Parse fixed ps metadata, including a start identity; never arguments."""
    if type(text) is not str or len(text.encode()) > MAX_BYTES:
        raise ObservationError("invalid process snapshot size")
    result = {}
    for line in text.splitlines():
        fields = line.split(None, 8)
        if len(fields) != 9 or not all(re.fullmatch(r"[0-9]+", x) for x in fields[:3]):
            raise ObservationError("malformed process metadata")
        pid, parent, uid = map(int, fields[:3])
        natural(pid); natural(parent, zero=True); natural(uid, zero=True)
        start = " ".join(fields[3:8])
        try:
            datetime.datetime.strptime(start, "%a %b %d %H:%M:%S %Y")
        except ValueError as error:
            raise ObservationError("invalid process start identity") from error
        comm = fields[8]
        if not comm or any(ord(c) < 32 for c in comm) or pid in result:
            raise ObservationError("ambiguous process metadata")
        result[pid] = dict(pid=pid, parent=parent, uid=uid, start=start, comm=comm)
        if len(result) > MAX_PROCESSES:
            raise ObservationError("process snapshot exceeds bound")
    if not result:
        raise ObservationError("empty process snapshot")
    return result


def parse_open_files(raw):
    """Pure parser for a complete NUL-field lsof response, not a collector."""
    if type(raw) is not str or len(raw.encode()) > MAX_BYTES or "\0" not in raw:
        raise ObservationError("incomplete open-file response")
    rows, current, pid = [], None, None
    for field in raw.split("\0"):
        field = field.lstrip("\n")
        if not field:
            continue
        key, value = field[0], field[1:]
        if key in "pf":
            if current is not None:
                if set(current) != {"pid", "fd", "device", "inode", "path"}:
                    raise ObservationError("incomplete open-file record")
                rows.append(current)
                current = None
            if key == "p":
                if not value.isdecimal():
                    raise ObservationError("invalid open-file PID")
                pid = natural(int(value))
                continue
            if pid is None or not re.fullmatch(r"(?:[0-9]+|cwd|rtd|txt|mem)", value):
                raise ObservationError("invalid open-file descriptor")
            current = {"pid": pid, "fd": value}
        elif current is None or key not in "Din":
            raise ObservationError("unexpected open-file field")
        elif key == "D":
            if "device" in current or not re.fullmatch(r"(?:0x)?[0-9a-fA-F]+", value):
                raise ObservationError("invalid open-file device")
            current["device"] = int(value, 16)
        elif key == "i":
            if "inode" in current or not value.isdecimal():
                raise ObservationError("invalid open-file inode")
            current["inode"] = natural(int(value))
        else:
            if "path" in current:
                raise ObservationError("duplicate open-file path")
            current["path"] = canonical_path(value)
    if current is not None:
        if set(current) != {"pid", "fd", "device", "inode", "path"}:
            raise ObservationError("incomplete open-file record")
        rows.append(current)
    if not raw.endswith(("\0", "\0\n")):
        raise ObservationError("truncated open-file response")
    return rows


def _watched(paths):
    result = []
    for row in paths:
        if type(row) is not dict or set(row) != {"path", "directory"} or type(row["directory"]) is not bool:
            raise ObservationError("invalid watched path")
        result.append((canonical_path(row["path"]), row["directory"]))
    if not result:
        raise ObservationError("empty release scope")
    return tuple(result)


def _matches(path, watched):
    path = canonical_path(path)
    # A deleted-file display may still be an open reference to the release.
    variants = (path, path[:-10]) if path.endswith(" (deleted)") else (path,)
    return any(p == root or (directory and p.startswith(root.rstrip("/") + "/"))
               for p in variants for root, directory in watched)


def _protocol_key(row):
    if type(row) is not dict or set(row) != {"pid", "fd", "device", "inode", "path"}:
        raise ObservationError("invalid protocol/open-file proof")
    natural(row["pid"]); natural(row["device"], zero=True); natural(row["inode"])
    if type(row["fd"]) is not str or not row["fd"].isdecimal():
        raise ObservationError("protocol descriptor must be numeric")
    canonical_path(row["path"])
    return tuple(row[k] for k in ("pid", "fd", "device", "inode", "path"))


def classify(before, after, facts, launch, paths, root, controls=(), protocol=()):
    """Classify trusted in-memory observations; this function grants nothing.

    Providers must supply per-process identity and complete scope coverage.
    This is a pure policy function used by fixtures. There is no snapshot-file
    option and production does not accept caller-supplied observation JSON.
    """
    if type(before) is not dict or not before or type(after) is not dict or type(facts) is not dict:
        raise ObservationError("invalid or empty process enumeration")
    for snapshot in (before, after):
        for pid, row in snapshot.items():
            if (type(row) is not dict or set(row) != {"pid", "parent", "uid", "start", "comm"} or
                    type(pid) is not int or row["pid"] != pid):
                raise ObservationError("invalid process identity")
            natural(pid); natural(row["parent"], zero=True); natural(row["uid"], zero=True)
            if not all(type(row[k]) is str and row[k] for k in ("start", "comm")):
                raise ObservationError("incomplete process identity")
    watched = _watched(paths)
    root = canonical_path(root)
    control = set(controls)
    if any(type(p) is not int or p not in before for p in control):
        raise ObservationError("unverified control PID")
    busy, unknown, reasons, seen_protocol = set(), set(), set(), set()
    proof_keys = {_protocol_key(p) for p in protocol}
    if len(proof_keys) != len(protocol):
        raise ObservationError("duplicate protocol proof")
    if before != after:
        reasons.add("process-snapshot-changed")
        unknown.update(set(before) | set(after))
    if set(facts) - set(before):
        raise ObservationError("observation contains an unenumerated process")
    references, cargo_locks = {}, {}
    for pid, identity in before.items():
        fact = facts.get(pid)
        if fact is None:
            unknown.add(pid); reasons.add("process-reference-coverage-unavailable")
            continue
        if (type(fact) is not dict or set(fact) != {"identity", "scopes", "paths", "files"} or
                fact["identity"] != identity or set(fact["scopes"]) != set(SCOPES) or
                set(fact["paths"]) != set(SCOPES) - {"files"} or type(fact["files"]) is not list):
            raise ObservationError("incomplete or stale process observation")
        for scope, state in fact["scopes"].items():
            if state not in {"complete", "denied", "unavailable"}:
                raise ObservationError("unknown observation status")
            if scope == "command" and pid in control:
                continue
            if state != "complete":
                unknown.add(pid); reasons.add("process-scope-" + state)
        refs = []
        for scope, items in fact["paths"].items():
            if type(items) is not list:
                raise ObservationError("invalid process reference list")
            for path in items:
                canonical_path(path); refs.append(path)
                if not (scope == "command" and pid in control) and _matches(path, watched):
                    busy.add(pid); reasons.add("process-references-release")
        keys, numeric_fds = set(), set()
        cargo_locks[pid] = []
        for row in fact["files"]:
            if type(row) is not dict or row.get("pid") != pid:
                raise ObservationError("open-file process identity mismatch")
            fd = row.get("fd")
            # Non-numeric cwd/text/map descriptors can never be protocol locks.
            if fd in {"cwd", "rtd", "txt", "mem"}:
                probe = dict(row, fd="0"); key = _protocol_key(probe)
                key = (key[0], fd, *key[2:])
            else:
                key = _protocol_key(row)
                if fd in numeric_fds:
                    raise ObservationError("descriptor changed within one observation")
                numeric_fds.add(fd)
                if row["path"].endswith("/.cargo-lock"):
                    cargo_locks[pid].append(row["path"])
            if key in keys:
                raise ObservationError("duplicate open-file observation")
            keys.add(key); refs.append(row["path"])
            if _matches(row["path"], watched):
                if key in proof_keys:
                    seen_protocol.add(key)
                else:
                    busy.add(pid); reasons.add("release-has-open-files")
        references[pid] = refs
    if seen_protocol != proof_keys:
        reasons.add("protocol-lock-coverage-incomplete")
        unknown.update(p[0] for p in proof_keys - seen_protocol)
    compiler_states = {}

    def compiler(pid, visited):
        if pid in compiler_states:
            return compiler_states[pid]
        if pid in visited or pid not in references or pid in unknown:
            value = "unknown"
        elif any(p == root or p.startswith(root.rstrip("/") + "/") for p in references[pid]):
            value = "busy"
        elif cargo_locks.get(pid):
            value = "elsewhere"
        else:
            parent = before[pid]["parent"]
            value = compiler(parent, visited | {pid}) if parent in compilers else "unknown"
        compiler_states[pid] = value
        return value

    compilers = {p for p, v in before.items() if PurePosixPath(v["comm"]).name in COMPILERS}
    for pid in compilers:
        state = compiler(pid, set())
        if state == "busy":
            busy.add(pid); reasons.add("project-compiler-active")
        elif state == "unknown":
            unknown.add(pid); reasons.add("compiler-attribution-unknown")
    if type(launch) is not dict or set(launch) != set(LAUNCH_SCOPES):
        raise ObservationError("incomplete launch-source inventory")
    for state in launch.values():
        if type(state) is not dict or set(state) != {"status", "paths"} or type(state["paths"]) is not list:
            raise ObservationError("invalid launch-source observation")
        if state["status"] not in {"complete", "denied", "unavailable"}:
            raise ObservationError("unknown launch-source status")
        if state["status"] != "complete":
            reasons.add("launch-reference-coverage-unavailable")
        if any(_matches(path, watched) for path in state["paths"]):
            reasons.add("launch-references-release")
    if "launch-references-release" in reasons:
        outcome = "busy"
    else:
        outcome = "busy" if busy else "unknown" if reasons or unknown else "idle"
    return dict(state=outcome, busy_pids=sorted(busy), unknown_pids=sorted(unknown),
                reasons=sorted(reasons), process_count=len(before))


def collect_metadata(root, paths, controls=(), protocol=()):
    """Use permitted ps metadata only. No implicit escalation to /proc or lsof.

    Reference and launch providers are not yet available under this execution
    grant. Their explicit absence is passed through the same complete policy.
    """
    def snapshot():
        with subprocess.Popen(PS_COMMAND, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"}, close_fds=True) as query:
            try:
                output, error = query.communicate(timeout=10)
            except subprocess.TimeoutExpired as failure:
                query.kill(); query.communicate()
                raise ObservationError("process metadata timed out") from failure
            if query.returncode or error.strip():
                raise ObservationError("process metadata unavailable")
            rows = parse_processes(output)
            own = rows.get(query.pid)
            if (own is None or own["parent"] != os.getpid() or own["uid"] != os.getuid()
                    or PurePosixPath(own["comm"]).name != "ps"):
                raise ObservationError("metadata query process identity is unconfirmed")
            # Only this exact no-inherited-FD metadata query is transient.
            del rows[query.pid]
            if not rows:
                raise ObservationError("empty process enumeration")
            return rows
    before = snapshot()
    after = snapshot()
    launch = {s: {"status": "unavailable", "paths": []} for s in LAUNCH_SCOPES}
    return classify(before, after, {}, launch, paths, root, controls, protocol)


def assert_idle(root, paths, verified_control_pids=(), *, verified_protocol_locks=(),
                protocol_validator=None):
    if protocol_validator is None:
        raise ObservationError("original protocol lock validator required")
    snapshots = protocol_validator(root, verified_protocol_locks)
    watched = [{"path": str(p), "directory": p.is_dir()} for p in paths]
    protocol = [dict(value["record"], fd=str(value["record"]["fd"]))
                for value in snapshots.values()]
    # The original proof includes private stat snapshots; only exact key fields
    # enter classification, and the original validator checks them again.
    protocol = [{k: row[k] for k in ("pid", "fd", "device", "inode", "path")} for row in protocol]
    result = collect_metadata(str(root), watched, verified_control_pids, protocol)
    protocol_validator(root, verified_protocol_locks, snapshots)
    if result["state"] != "idle":
        raise ObservationError("Linux release observation " + result["state"] + ": " +
                               ",".join(result["reasons"]))
    return []
