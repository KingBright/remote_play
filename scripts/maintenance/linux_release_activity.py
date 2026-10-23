"""Linux release activity policy and metadata-only collector.

No direct /proc reads, privilege changes, or receipt import. Scoped OS queries
keep command arguments local; denied or incomplete references never mean idle.
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



# Production release queries mirror the original fixed-path checks. The strict
# per-process dossier above remains a fixture classifier, not a release contract.
PS_ARGUMENTS = ("/bin/ps", "-eo", "pid=,args=")
LSOF = "/usr/bin/lsof"
SYSTEMD_ANALYZE = "/usr/bin/systemd-analyze"


def _reference_query(command, *, empty_code=False):
    """Fixed internal OS queries; never return raw diagnostics to the caller."""
    env = {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CONFIG_DIRS", "XDG_DATA_HOME",
                "XDG_DATA_DIRS", "XDG_RUNTIME_DIR", "SYSTEMD_UNIT_PATH"):
        if key in os.environ:
            env[key] = os.environ[key]
    try:
        with subprocess.Popen(command, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8",
                errors="strict", close_fds=True, env=env) as query:
            try:
                output, error = query.communicate(timeout=20)
            except subprocess.TimeoutExpired as failure:
                query.kill(); query.communicate()
                raise ObservationError("release reference query timed out") from failure
            if (query.returncode not in ((0, 1) if empty_code else (0,)) or
                    error.strip() or len(output.encode()) > MAX_BYTES or
                    (query.returncode == 1 and output)):
                raise ObservationError("release reference query incomplete")
            if tuple(command) == PS_COMMAND:
                own = parse_processes(output).get(query.pid)
                if (own is None or own["parent"] != os.getpid() or
                        own["uid"] != os.getuid() or own["comm"] != "ps"):
                    raise ObservationError("metadata query identity unconfirmed")
                output = "\n".join(line for line in output.splitlines()
                                  if int(line.split(None, 1)[0]) != query.pid) + "\n"
            return output
    except (OSError, UnicodeError, subprocess.SubprocessError) as error:
        raise ObservationError("release reference query unavailable") from error


def _command_rows(text):
    rows = {}
    for line in text.splitlines():
        parts = line.strip().split(None, 1)
        if not parts or not parts[0].isdecimal() or int(parts[0]) in rows:
            raise ObservationError("invalid process command coverage")
        pid = natural(int(parts[0]))
        rows[pid] = parts[1] if len(parts) == 2 else ""
    if not rows or len(rows) > MAX_PROCESSES:
        raise ObservationError("empty or unbounded process command coverage")
    return rows


def _launch_roots(user):
    command = [SYSTEMD_ANALYZE]
    if user:
        command.append("--user")
    command.append("unit-paths")
    text = _reference_query(command)
    result = []
    for line in text.splitlines():
        canonical_path(line)
        if line not in result:
            result.append(line)
    if not result or len(result) > 64:
        raise ObservationError("systemd launch search path coverage unavailable")
    return result


def _launch_values(text):
    import shlex
    # Interpret quoting/specifiers as data, never execute a unit or desktop entry.
    text = text.replace("\\\n", "")
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith(("#", ";", "[")) or "=" not in line:
            continue
        name, value = line.split("=", 1)
        value = re.sub(r"\\x([0-9a-fA-F]{2})", lambda m: chr(int(m[1], 16)), value)
        yield value
        if not (name.startswith("Exec") or name in {"Environment", "EnvironmentFile"} or
                name.endswith(("Path", "Paths", "Directory", "Directories"))):
            continue  # Descriptions are literal text, not shell argument syntax.
        try:
            yield from shlex.split(value)
        except ValueError as error:
            raise ObservationError("ambiguous launch configuration quoting") from error



def same_file_identity(before, after):
    # Reading may update atime; it is not content/ownership identity.
    keys = ("st_dev", "st_ino", "st_mode", "st_uid", "st_gid", "st_nlink",
            "st_size", "st_mtime_ns", "st_ctime_ns")
    return all(getattr(before, key) == getattr(after, key) for key in keys)



def vendor_unit_link(link, target, info):
    """Only root-owned systemd aliases and targets owned by the same RPM."""
    import stat
    from pathlib import Path
    if info.st_uid != 0 or not str(link).startswith("/usr/lib/systemd/"):
        raise ObservationError("launch link outside reviewed search paths")
    if target.suffix not in {".service", ".socket", ".target", ".timer", ".path",
                             ".mount", ".automount", ".slice", ".scope", ".conf"}:
        raise ObservationError("vendor launch link is not a unit")
    for part in (target, *target.parents):
        state = part.lstat()
        if (state.st_uid != 0 or state.st_mode & 0o022 or
                (part == target and not stat.S_ISREG(state.st_mode)) or
                (part != target and not stat.S_ISDIR(state.st_mode))):
            raise ObservationError("vendor launch link ownership or mode is unsafe")
    packages = []
    for path in (link, target):
        text = _reference_query(["/usr/bin/rpm", "-qf", "--queryformat",
                                 "%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}\n", str(path)])
        package = text.strip()
        if not re.fullmatch(r"[A-Za-z0-9_.+:%~^-]+", package):
            raise ObservationError("vendor launch package ownership is unconfirmed")
        packages.append(package)
    if packages[0] != packages[1]:
        raise ObservationError("vendor launch link package differs from target")


def launch_reference_scan(paths):
    """Static systemd/XDG references; preserve original root-only warnings."""
    from pathlib import Path
    import pwd
    import stat
    home = str(Path.home().resolve())
    uid = os.getuid()
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/run/user/" + str(uid))
    user_name = pwd.getpwuid(uid).pw_name
    roots = [(p, False, False) for p in _launch_roots(True)]
    roots += [(p, True, False) for p in _launch_roots(False)]
    config = os.environ.get("XDG_CONFIG_HOME", home + "/.config")
    configs = os.environ.get("XDG_CONFIG_DIRS", "/etc/xdg").split(":")
    roots += [(config + "/autostart", False, True)]
    roots += [(p + "/autostart", True, True) for p in configs]
    watched = _watched(paths)
    allowed = [Path(canonical_path(p)).resolve() for p, _, _ in roots]
    warnings, hits, count, byte_count = [], [], 0, 0
    suffixes = {".service", ".socket", ".target", ".timer", ".path",
                ".mount", ".automount", ".slice", ".scope", ".conf"}
    seen = set()
    for raw, system, desktop in roots:
        folder = Path(canonical_path(raw))
        if not folder.exists():
            if folder.is_symlink():
                raise ObservationError("dangling launch search path")
            continue
        source_root = folder
        root_identity = source_root.lstat()
        folder = source_root.resolve()
        if not folder.is_dir():
            raise ObservationError("invalid launch search directory")
        def visit(directory, depth):
            nonlocal count, byte_count
            if depth > 16:
                raise ObservationError("launch directory depth exceeded")
            for item in sorted(directory.iterdir()):
                info = item.lstat()
                if stat.S_ISDIR(info.st_mode):
                    if not desktop:
                        visit(item, depth + 1)
                    continue
                if (desktop and item.suffix != ".desktop") or (
                        not desktop and item.suffix not in suffixes):
                    continue
                count += 1
                if count > 20000:
                    raise ObservationError("launch inventory bound exceeded")
                target = item
                if stat.S_ISLNK(info.st_mode):
                    target = item.resolve()
                    if str(target) == "/dev/null":  # systemd mask
                        continue
                    if not any(target == p or p in target.parents for p in allowed):
                        vendor_unit_link(item, target, info)
                try:
                    current = target.stat()
                    if not stat.S_ISREG(current.st_mode):
                        raise ObservationError("nonregular launch configuration")
                    key = (current.st_dev, current.st_ino)
                    if key in seen:
                        continue
                    with target.open("rb") as stream:
                        data = stream.read(MAX_BYTES + 1)
                    if not same_file_identity(current, target.stat()) or not same_file_identity(info, item.lstat()):
                        raise ObservationError("launch configuration changed")
                except PermissionError as error:
                    if (not system or not stat.S_ISREG(info.st_mode) or
                            info.st_uid != 0 or not same_file_identity(info, item.lstat())):
                        raise ObservationError("launch configuration permission denied") from error
                    warnings.append({"path": str(item),
                        "reason": "unreadable-root-owned-global-launch-config",
                        "inspection_complete": False})
                    continue
                seen.add(key); byte_count += len(data)
                if len(data) > MAX_BYTES or byte_count > 32 * MAX_BYTES:
                    raise ObservationError("launch configuration bound exceeded")
                try:
                    text = data.decode("utf-8")
                except UnicodeError as error:
                    raise ObservationError("launch text unavailable") from error
                values = list(_launch_values(text))
                for value in values:
                    for token, replacement in (("%h", home), ("%t", runtime),
                                                ("%u", user_name), ("%U", str(uid))):
                        value = value.replace(token, replacement)
                    candidates = [value]
                    absolute = value.split("=", 1)[-1].lstrip("-@:+!")
                    if absolute.startswith("/"):
                        candidates.append(str(Path(absolute).resolve()))
                    if any(any(root in p for root, _ in watched) for p in candidates):
                        hits.append(str(item))
        visit(folder, 0)
        if (not same_file_identity(root_identity, source_root.lstat()) or
                source_root.resolve() != folder):
            raise ObservationError("launch search directory changed")
    return {"hits": sorted(set(hits)), "warnings": warnings,
            "configurations": len(seen), "search_roots": len(roots)}


def _compiler_paths(raw):
    """Only PID and path facts; sockets/anonymous objects cannot be Cargo locks."""
    rows, pid = {}, None
    if raw and not raw.endswith(("\0", "\0\n")):
        raise ObservationError("truncated compiler reference query")
    for field in raw.split("\0"):
        field = field.lstrip("\n")
        if not field:
            continue
        if field.startswith("p") and field[1:].isdecimal():
            pid = natural(int(field[1:])); rows.setdefault(pid, [])
        elif field.startswith("n") and pid is not None:
            if field[1:].startswith("/"):
                rows[pid].append(canonical_path(field[1:]))
        else:
            raise ObservationError("invalid compiler reference response")
    return rows


def collect_release_references(root, paths, controls=(), protocol=()):
    """Original command/compiler/scoped-FD/launch checks, no global map dossier.

    OS queries run locally; command arguments are neither returned nor persisted.
    A denied scoped lsof query is a refusal, never negative reference evidence.
    """
    from pathlib import Path
    root = canonical_path(root)
    watched = _watched(paths)
    before = parse_processes(_reference_query(PS_COMMAND))
    commands = _command_rows(_reference_query(PS_ARGUMENTS))
    after = parse_processes(_reference_query(PS_COMMAND))
    reasons, busy = set(), set()
    controls = set(controls) | {os.getpid()}
    for pid, identity in after.items():
        if pid in controls:
            continue
        previous = before.get(pid, {})
        # comm is a mutable display label (for example Linux kworker names).
        # It is not the process identity that binds this argument snapshot.
        if not commands.get(pid) or any(previous.get(k) != identity[k]
                for k in ("pid", "parent", "uid", "start")):
            reasons.add("process-command-identity-unconfirmed")
            continue
        if any(path in commands[pid] for path, _ in watched):
            busy.add(pid); reasons.add("process-references-release")
    # Only compiler candidates need project attribution. Unrelated processes
    # never need an exe/cwd/map/open-descriptor dossier.
    candidates = {p: row for p, row in after.items()
                  if PurePosixPath(row["comm"]).name in COMPILERS}
    if candidates:
        raw = _reference_query([LSOF, "-nP", "-a", "-p",
            ",".join(map(str, sorted(candidates))), "-F0pn"], empty_code=True)
        refs = _compiler_paths(raw)
        settled = parse_processes(_reference_query(PS_COMMAND))
        states = {}
        def compiler(pid, visited):
            if pid in states:
                return states[pid]
            if (pid in visited or pid not in refs or
                    candidates[pid] != settled.get(pid)):
                result = "unknown"
            elif any(p == root or p.startswith(root + "/") for p in refs[pid]):
                result = "busy"
            elif any(p.endswith("/.cargo-lock") for p in refs[pid]):
                result = "elsewhere"
            elif candidates[pid]["parent"] in candidates:
                result = compiler(candidates[pid]["parent"], visited | {pid})
            else:
                result = "unknown"
            states[pid] = result
            return result
        for pid in candidates:
            state = compiler(pid, set())
            if state == "busy":
                busy.add(pid); reasons.add("project-compiler-active")
            elif state == "unknown":
                reasons.add("compiler-attribution-unknown")
    proof = {_protocol_key(p) for p in protocol}
    if len(proof) != len(protocol):
        raise ObservationError("duplicate protocol proof")
    required, seen = set(), set()
    for name, directory in watched:
        path = Path(name)
        if not path.exists():
            if path.is_symlink():
                raise ObservationError("dangling watched release path")
            continue
        if path.is_symlink() or path.is_dir() != directory:
            raise ObservationError("watched release path changed kind")
        required.update(key for key in proof if _matches(key[4], ((name, directory),)))
        command = [LSOF, "-nP", "-F0pfDin"]
        command += ["+D", name] if directory else ["--", name]
        raw = _reference_query(command, empty_code=True)
        for row in parse_open_files(raw) if raw else []:
            if not _matches(row["path"], ((name, directory),)):
                raise ObservationError("open-file query escaped release scope")
            if row["fd"].isdecimal() and _protocol_key(row) in proof:
                seen.add(_protocol_key(row))
            else:
                busy.add(row["pid"]); reasons.add("release-has-open-files")
    if required != seen:
        reasons.add("protocol-lock-coverage-incomplete")
    launch = launch_reference_scan(paths)
    if launch["hits"]:
        reasons.add("launch-references-release")
    state = ("busy" if busy or launch["hits"] else "unknown" if reasons else "idle")
    return {"state": state, "reasons": sorted(reasons), "busy_pids": sorted(busy),
            "process_count": len(after), "warnings": launch["warnings"]}

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
    result = collect_release_references(str(root), watched, verified_control_pids, protocol)
    protocol_validator(root, verified_protocol_locks, snapshots)
    if result["state"] != "idle":
        raise ObservationError("Linux release observation " + result["state"] + ": " +
                               ",".join(result["reasons"]))
    return result.get("warnings", [])
