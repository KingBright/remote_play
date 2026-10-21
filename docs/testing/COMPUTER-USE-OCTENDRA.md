# Octendra → selected Weavelo source: observe-only bridge

This route consumes an existing, explicit Octendra desktop authorization. It does
not make terminal/code permission into desktop permission, start a stream, choose
a default source, grant OS permissions, create credentials or open a listener.
Only an already selected, visible, paired viewing session can supply pixels.
Input requests remain unavailable; USB HID usages are not platform keycodes.

The Octendra executor UUID stays in `identity.device_id`. The separately approved
`weavelo.target` contains the Weavelo peer ID, typed capture source and exact
connection ID. The backend desktop ID is
`weavelo:<connection>:<main|display:id|window:id>:<peer>`. These are distinct identity
domains; the remote peer is never substituted for the executor.

## Existing authority and private transport

Octendra's existing `desktop-launch.json` gains an optional `weavelo` route. Old
local-window grants do not authorize this route. The route requires observe=true,
input=false, desktop_session=`weavelo`, an exact target, current GUI generation,
GUI executable digest and profile. Existing worker/Agent executable pins,
owner/task/capability checks, operation journal and short leases remain in use.

The GUI is closed to this bridge by default. Its actual private adapter directory
(profile or profile/gui-state, as reported by the existing profile ownership
logic) may contain an owner-only `octendra-authority.json` locator:

```json
{"desktop_launch":"/absolute/existing/octendra-state/desktop-launch.json"}
```

This file only pins the location of the existing authorization; it is not a new
grant or credential. Neither file is created or enabled automatically. Both
directories must be private to the same user (0700); authority records must be
regular non-symlink, single-link files with mode 0600. There is no request-supplied
authority path, automatic grant discovery or profile fallback.

Before an operation, the GUI compares the current launch authority and the
existing per-task `desktop-worker-<open-operation-id>.json` record. It checks the
owner, task, executor, capability version, peer/source, generation, bytes and
expiry. The socket's kernel peer PID identifies the real worker; executable pins
and the actual approved Agent parent are checked using fixed process metadata
and executable paths, never process arguments. Cached-frame export revalidates
through that same Agent/worker relationship. Missing or changed authority fails
closed. Source/frame freshness and the original connection/source fence are
checked independently by the Weavelo owner.

The existing `gui-receive.sock` carries bounded JSON metadata followed by bounded
raw frame bytes. Partial request reads retain the existing two-second timeout;
only observation completion has the longer native-readback budget. The GUI event
loop and session lock are not held during native completion. A monotonic watchdog
retires the observer and retained frame even if its client disappears.

## Pixel and lease contract

Pixels come from the existing decoded frame. macOS 8-bit NV12 uses Core Image to
produce `rgba8_srgb`; software buffers retain `rgba8` with unspecified color.
Packed RGBA has one tightly packed plane. Its encoding declares the color
semantics; the legacy SPA YUV metadata array is zero, not fabricated. Maximum
output is the smaller of the caller's bound and 16 MiB. There is no implicit
scaling/cropping; oversized images fail.

The mapping is the source-local pixel canvas: frame size and logical canvas size
are the actual raster size, rotation is zero and canvas origin is [0,0]. That
origin does not claim a remote monitor's global position or DPI. Configure the
actual selected source/raster dimensions rather than guessing window geometry.

Frame TTL starts from the actual decoded frame timestamp. Keeping a frame does
not preserve authority. Selection, source revision, connection allocation, group,
input grant, visibility, service epoch and expiry changes invalidate the original
fence; cached bytes do not bypass that check. No lease is renewed automatically.

## Validation boundary

`python3 -B scripts/test_stream_settings_model.py --mode computer-use` compiles
the actual GPUI/Tokio application library and checks receipt boundaries, authority
matching and private-socket framing/PID attribution. Policy and IPC fixtures use
synthetic identities/bytes and do not constitute live desktop authorization.
`computer-use-pixels` separately tests actual CoreVideo/CoreImage readback with
synthetic IOSurface-backed buffers. No test captures a private screen.

This source increment does not install either product, alter a live profile,
activate a capability, establish live remote-device acceptance or complete input
dispatch. Use ordinary signed installation and the existing authorization process
before live observation acceptance. Real input remains out of this route.
