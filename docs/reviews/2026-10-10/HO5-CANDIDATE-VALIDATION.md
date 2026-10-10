# HO5 candidate source delivery and Linux validation

Implementation candidate `90fcce0a474abcd29369f216e331d7d240fecc30` was committed,
pushed and verified at `refs/heads/codex/gpui-mvvm-migration` on GitHub. HO5 fetched
it normally into `/var/home/liang/workspace/remote_play`, retaining its previous
`main@969ca109`. Its new branch was clean. All 938 manifest files matched, and
891 changed files had their mtimes refreshed before the first build. Existing
`target` was reused. No new MCP file transfer or source-policy workaround ran.

At 08:22:43 UTC HO5 was reachable through Remote Hosts, boot ID
`2788a001-2ddf-453b-b616-b58198a9ae3f`, different from old
`7cdb1ab9-985d-4b40-b4ee-fed5d587163a`. Uptime was 1911 seconds, booted deployment
`44.20261006.1`, staged false, transaction null. Agent 0.10.25 was compatible and
its user service active/running, PID 2019. All six queried SDK pkg-config checks
passed. These are fresh host observations, not inference from gateway presence.

## Completed checks

- `cargo check -p host --lib --locked`: passed on Linux. The initial offline
  attempt lacked PipeWire in Cargo cache; ordinary locked Cargo download through
  the existing registry configuration resolved it without changing the lockfile.
- `cargo check -p remote_play_app --bin remote_play --locked --offline`: passed.
- Same check with `--features native-linux-video`: passed, including the C ABI
  bridge and fixed GPUI/Blade native interop. This is a type/build check, not a
  linked runnable executable or actual frame presentation.
- `cargo test -p host --lib linux_pipewire::tests --locked --offline`: 11 passed,
  zero failed/ignored. Initial linking failed on `-lopus`: the existing vendored
  build generated `out/lib64/libopus.a` while audiopus_sys emitted `out/lib`.
  The successful test used that exact build's `lib64` via command-local
  `LIBRARY_PATH`, without system symlinks, package installation or source edits.

Existing FFmpeg 8.1.2 cache tarball SHA-256 is
`464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c`.
Signature verification returned VALIDSIG for
`FCF986EA15E6E293A5644F10B4322F04D67658D8`, matching the
[official release key](https://ffmpeg.org/download.html). 905 libavcodec/libavutil
source headers matched the signed archive; avconfig.h was generated separately.
Headers are codec 62.28.102/util 60.26.102; installed runtime calls report
62.28.103/60.26.103. No system FFmpeg/driver/configuration was replaced.

## Running product and remaining acceptance

`remote-play-current.service` was active/running with PID 3346, started at
2026-10-10 15:51:05 CST. `/proc/3346/exe` readlink returned Permission denied.
No alternate route was used; actual process executable, build and source remain
unknown. The installed service was not changed or restarted by this task.

No new candidate GUI was started, packaged, installed or published. Actual
portal choice/capture, stream, native presentation, input, audio, file transfer,
visual comparison and sustained performance remain pending. The owner's UI
deployment hold applies. MacBook old-GUI findings are recorded separately in
`MACBOOK-UI-IDENTITY-INCIDENT.md`; Mac/Windows acceptance is not counted here.
The source/build/package/process/UI chain still needs the requested complete
launch/install gate; current product-info and renderer checks do not carry
compiled commit/build or prove visual equivalence. A read-only launchd lifecycle
checker and six regression tests reject the reported unconditional GUI restart
policy while permitting headless background KeepAlive. It does not modify or
stop any service and is not yet integrated into installed launchers.

Durable operation identities and exact command conditions are recorded in
`evidence/ho5-candidate-validation.json`. This report's commit adds documentation
and the Python lifecycle checker/tests; all tested Rust source bytes remain
unchanged. The manifest now includes those two support files (940 total), with
the earlier 938-file digest retained in the Linux test receipt. Local required
Python regression after the helper change: 197 tests, zero failures, six native
integration skips. This local run includes preserved Android release tests that
are outside the committed Linux candidate.
