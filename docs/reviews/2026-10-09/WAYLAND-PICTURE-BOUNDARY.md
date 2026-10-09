# Wayland picture boundary, 2026-10-09

**The requested source/viewer image pair was not obtained.** This attempt stopped before safe capture. Black frames, frozen content, complete Wayland desktop coverage and actual GPUI visible presentation remain unknown. Earlier decoding and GPUI scene counters in [the source-selection milestone](SOURCE-SELECTION-MILESTONE.md) retain their original limited meaning.

## Actual bounded observation

HO5 ran one task-owned GTK4 native Wayland window, titled `RemotePlay owned Wayland picture validation`, for approximately eight seconds. Its 640×360 surface was mapped as `GdkWaylandToplevel` on `wayland-0`, with 16 drawing callbacks and no X11 surface ID. The window destroyed itself and the application exited 0; a separate final process check confirmed its PID 2269810 gone. This establishes a native Wayland test surface and bounded cleanup. It does not establish capture, visible drawing, streaming or display completion.

The test only read X11 root geometry and portal properties. It did not capture desktop pixels, enumerate unrelated window contents, inject input or request permissions. ScreenCast portal properties were version 5, AvailableSourceTypes 7 and AvailableCursorModes 7. Availability alone proves neither authorization nor product integration; no portal session or stream method was called.

The read-only Mutter display-state query returned ServiceUnknown. Physical primary-output identity, physical resolution and compositor identity were not established. X11 root metadata was 3840×2160 at this attempt. The prior actual product capture command used `x11grab -video_size 1920x1080 -i :0`. These observations are not simultaneous: they expose a capture-rectangle/cropping risk but do not prove the exact content of the earlier stream.

On Studio, the external helper's screen-capture preflight and accessibility-trust query both returned false. These results apply only to that helper, not to the signed product; there was no permission request or TCC change. The existing signed GPUI binary has no bounded presented-image export. The uncompiled `capture_restored_window` example exports its own decoded RGBA and requires a peer's exact Window/title/PID, so it would not establish what GPUI actually presented. The host also excludes its own PID from window enumeration. No new helper or product build was made.

The original HO5 receipt, its SHA-256, complete 16 samples, portal metadata, helper results and final process checks are embedded in [wayland-picture-boundary.json](evidence/wayland-picture-boundary.json). Image paths, frame hashes and luminance are null because no image pair exists.

## Linux implementation boundary

| Component | Inspected behavior | Consequence |
| --- | --- | --- |
| `host/src/ffmpeg_hevc.rs:184` | If DISPLAY exists, use x11grab; otherwise kmsgrab. No PipeWire/portal input. | A Wayland session with DISPLAY takes the X11 compatibility path. Full compositor content is unverified. |
| Same command builder | Requested output dimensions become X11 input video_size. The scale filter is Windows-only. | A 1920×1080 request does not establish downscaling of a larger Linux desktop. |
| `host/src/capture_sources.rs:165` | Generic MainDisplay/Desktop, 0×0, no process identity, supports_input=true. | This catalog does not prove the physical primary display, capture readiness or complete Wayland content. It cannot select the owned native Wayland window. |
| `host/src/service.rs:351,827` | MainDisplay skips catalog validation; window_capture is macOS-only. | Existing Linux window capture is not available. MainDisplay's label is insufficient as a physical-source proof. |
| `host/src/linux_capture.rs:103` | LinuxVideoCapturer emits empty placeholder buffers. | It is not a native Wayland capture implementation. |
| `host/src/linux_video_encode.rs:33,128` | Starts the real FFmpeg source; submit_frame ignores those placeholder buffers. | The prior decoded stream came from FFmpeg. Empty placeholder buffers are not evidence that the decoded stream was black. |
| `host/src/service.rs:685`, `host/src/linux_input.rs:90` | Desktop injection lazily opens /dev/uinput or /dev/input/uinput. | Input may depend on existing device permissions. supports_input=true is not a readiness test; no input device was opened here. |

The GUI/source capability and readiness gaps remain open. This checkpoint changes only the misleading log; it does not claim to fix those declarations or add user selection of native Wayland sources.

## Small completed fix and verification

The old logger reported `pipewire/wayland` whenever WAYLAND_DISPLAY existed, even when the real command used x11grab. The new std-only `capture_backend` module reads the actual command's input format before its first `-i`. It reports x11grab, kmsgrab, gdigrab or unknown; encoded HEVC output format cannot override the input label. The command builder, source defaults, source catalog, wire capabilities, renderer, input and dependencies are unchanged.

`CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo test -p host --lib capture_backend::tests --offline --locked` passed **3/3**. Tests cover a Wayland environment with an X11 command, all existing input formats with HEVC output, and missing/unimplemented formats. They create no capture process or FFmpeg child. The parser compiled on local macOS through cfg(test); Linux/Windows production integration was not freshly built. The only emitted warning was the pre-existing unused PermissionsExt import. Already-passed suites were not repeated.

There is no new production executable, package or deployment for this logging change. The installed HO5 build can still emit the old misleading label. The retained signed candidate and rollback bundle from the preceding milestone were not altered.

## Minimum follow-up proposal

The next capture change should be reviewed as one concrete plan, because it affects native media/source semantics and permissions:

1. Make source and backend diagnostics accurate: distinguish X11 compatibility capture, native portal availability, actual capture readiness, selected output/rectangle and encoded dimensions. Do not advertise complete Wayland capture from environment variables or infer permission from a generic Desktop item. Define any needed capability/protocol change explicitly before implementation.
2. Add native Wayland ScreenCast portal selection through a normal user-approved picker and a bounded session. Bind the chosen PipeWire node and source metadata to the current peer/session/source revision; cancellation and close must release resources. Preserve input isolation, authentication and native frame buffers/color metadata through encoding. Do not silently fall back or make persistent permission/security changes.
3. With an explicitly selected task-owned dynamic window and an authorized capture/readback path on the actual GPUI viewer, save a few time-correlated source and presented-viewer images. Record source identity, source/encoded/presented sizes, frame hashes, luminance, changing visual marks and timestamps. Keep decoded-frame and GPUI-submission metrics separate from presented-image evidence. A helper-decoded screenshot alone cannot close this acceptance gap.

No part of this native portal proposal was implemented or enabled during this attempt.

## Final state

Studio remains on its canonical signed build 20261004.80 at PID 59676; its installed executable SHA-256 is unchanged. HO5 remains active/running at PID 2232948, NRestarts 0, unchanged installed executable SHA-256 and no direct FFmpeg child. The owned Wayland fixture is gone. Both final observations and hashes are in the evidence JSON.

All 105 unrelated dirty paths retain their bytes and status. The existing twelve-direction matrix remains unchanged (no new pass). No persistent configuration, membership, launch agent, OS security or permission grant was changed. There was no NAS publication or push.
