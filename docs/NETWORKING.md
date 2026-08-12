# Networking And EasyTier Integration

## Decision

RemotePlay will integrate EasyTier as a bundled sidecar process first.

The app will ship or install an `easytier-core` binary, generate a private mesh identity, and start EasyTier on launch with an app-owned config. Direct library integration is deferred until the runtime is stable across macOS, Windows, and Linux packaging.

## Why Sidecar First

- It keeps GPL/LGPL-style dependency and release risks outside the realtime media crates.
- It gives a clean rollback path: if EasyTier fails, direct LAN or manually configured host addresses can still work.
- It fits the cross-platform target. Each platform can package, sign, and permission the EasyTier binary using native installer rules while the RemotePlay protocol stays unchanged.
- It avoids adding any mandatory copies, proxies, or buffering to video/audio/control/data-plane packets. RemotePlay still sends UDP/data-plane traffic directly to the selected peer address; EasyTier only provides reachability.

## Relay Fallback

EasyTier remains the preferred cross-network path when it can establish a real peer route, but RemotePlay now has a first relay fallback primitive for networks where the shared EasyTier bootstrap is unavailable. The initial implementation is intentionally outside the GUI path:

- `remote_core::relay` provides a lightweight relay packet envelope plus local tunnels.
- `udp_relay_server` / `udp_relay_tunnel` keep the original RemotePlay UDP packet shape between the app and the local tunnel, then forward through a UDP relay server.
- `tcp_relay_server` / `tcp_relay_tunnel` provide a reachability-first fallback over persistent TCP connections. This is the conservative path for control, clipboard, file transfer, and emergency media fallback when UDP reachability is blocked.

The tunnel design is transparent to the current host/client protocol: a passive host can stay bound to `127.0.0.1:<port>`, a local tunnel forwards relay traffic into that socket, and the host replies to the tunnel address as if it were the client. The viewer side sends to its local tunnel instead of the remote peer address. This lets the existing control, RTP media, and data-plane packets cross a relay without changing their internal format.

The unified runtime can now opt into the first TCP relay fallback slice with:

```text
REMOTE_PLAY_RELAY=1
REMOTE_PLAY_RELAY_SERVER_ADDR=<relay-host:port>
```

When enabled, the app starts two local TCP relay tunnels automatically:

- a control tunnel for the existing RemotePlay UDP control/media/data packets
- a discovery tunnel so paired devices can announce themselves through the relay

Discovery packets received from the local relay discovery tunnel are converted into relay route candidates whose connect endpoint is the local relay control tunnel. The device model keeps direct and relay candidates for the same device and selects routes in this order: direct LAN/EasyTier first, then relay. The GUI intentionally labels devices as `Connectable`, `Online`, or `Offline` instead of exposing relay/LAN/EasyTier jargon in the main row.

Development overrides:

```text
REMOTE_PLAY_RELAY_CONTROL_BIND_ADDR=127.0.0.1:0
REMOTE_PLAY_RELAY_DISCOVERY_BIND_ADDR=127.0.0.1:0
REMOTE_PLAY_RELAY_LOG=1
```

The current relay server still needs to be supplied by deployment or a dev command. UDP relay remains available in `remote_core::relay`, but the unified runtime currently wires TCP relay first because it is the reachability-first fallback for restricted networks.

## Initial Runtime Shape

RemotePlay owns a mesh config containing:

- `network_name`: random per device group.
- `network_secret`: random per device group and always redacted in debug output.
- `node_id`: random per local install.
- `display_name`: user-facing device name.
- `initial_peers`: EasyTier peer URLs, defaulting to the public shared node for early bring-up.
- `auto_start`: whether the app should start EasyTier automatically.

The first sidecar command plan uses:

```text
easytier-core \
  --network-name <network_name> \
  --network-secret <network_secret> \
  --hostname <display_name> \
  --instance-name remote-play \
  --ipv4 <stable_app_derived_ipv4> \
  --latency-first true \
  --rpc-portal 127.0.0.1:15888 \
  -p tcp://public.easytier.cn:11010
```

The stable IPv4 is derived from the group `network_name` plus local `node_id` inside `10.128.0.0/10`: the group chooses a stable `/24`, and each device chooses a stable host address inside that subnet. This keeps devices in the same group routable with EasyTier's default `/24` TUN route while preserving a stable address across restarts. `--latency-first` matches RemotePlay's latency priority. `--private-mode` is not enabled for the default public shared-node path because EasyTier shared nodes are intentionally a foreign relay/discovery network; group membership is still guarded by `network_secret`.

## Sidecar Binary Discovery

The runtime now has a deterministic sidecar locator. It searches in this order:

1. `REMOTE_PLAY_EASYTIER_BIN`, for development or emergency override.
2. App-packaged resource locations next to the current executable, including macOS `.app/Contents/Resources` style paths.
3. The current executable directory.
4. `PATH`, for development machines.

An explicit `REMOTE_PLAY_EASYTIER_BIN` must point to a real binary; the app will report that mistake instead of silently falling back to another copy. On Unix platforms the locator also verifies the executable bit, so packaging permission mistakes fail early. Launch diagnostics use redacted command arguments so the mesh secret is never printed.

## Lifecycle Skeleton

`remote_core::mesh::EasyTierSidecarManager` can now build the launch plan, start the sidecar process, stop it, and best-effort kill it on drop.

Development runs can opt into the sidecar with:

```text
REMOTE_PLAY_MESH=1
```

When enabled, the unified app calls `load_or_generate`, locates `easytier-core`, starts or monitors the sidecar, and keeps the process handle alive for the app lifetime. `REMOTE_PLAY_MESH_DIR` can override the fallback config directory for local testing. Missing binaries or startup failures are reported as mesh errors and do not change the default LAN path when the flag is off.

On macOS packages, EasyTier is handled by a root LaunchDaemon after the user completes `Setup Mesh`. The LaunchDaemon runs `remote_play --mesh-daemon-run` rather than a static `easytier-core` command. That root process reads the user's mesh config, starts EasyTier with the current group, polls for config changes, and restarts EasyTier automatically after `Join Clipboard` or `New Group`. The plist therefore no longer stores the mesh network secret.

The runtime no longer depends on automatic `easytier-cli node` probes for normal health updates. RemotePlay assigns EasyTier a stable IPv4 up front and reports that expected address while process health is checked through the managed sidecar or root daemon path. CLI probing is kept as a bounded diagnostic/test capability, but it is not spawned repeatedly by the packaged app because the CLI has proven less stable than the sidecar process on macOS.

`EasyTierHealthSnapshot` now reports:

- process state: not started, running, or exited
- health state: starting, ready, degraded, or stopped
- virtual IP when known
- a short user-facing status message

`EasyTierRestartBackoff` provides the restart/backoff policy primitive. The unified app hands a started sidecar to `EasyTierHealthMonitorHandle` during development runs, or treats the root LaunchDaemon as the authoritative sidecar in packaged macOS runs without launching a second user-side process. Both paths publish `EasyTierHealthSnapshot` through a watch channel. This keeps the setup path automatic while preserving the direct RemotePlay packet path.

The unified Devices screen now shows the current mesh health snapshot when mesh is enabled: starting, ready with virtual IP, degraded, stopped, or needs admin setup.

## Pairing Model

The first app instance creates a device group and displays an invite payload:

```text
RPM2-<copy-friendly grouped code>
```

Another device joins by importing that payload. It receives the same network name, secret, and bootstrap peers, but generates its own `node_id` and display name.

The `RPM2` code is meant for copy/paste, AirDrop, chat, email, or a future QR view. It is grouped and case-insensitive, tolerates whitespace and hyphens, includes a checksum, and omits the default EasyTier public peer from the encoded bytes to keep the code reasonably compact. It is still longer than a consumer "6 digit" code because the offline invite must carry the mesh secret. A truly short code should use a temporary pairing broker or signed-in account sync, where the short code points to an encrypted invite for a few minutes.

The legacy payload is still accepted for compatibility:

```text
rpmesh1|<network_name>|<network_secret>|<peer1,peer2,...>
```

The client Devices screen now exposes this first pairing surface:

- `Copy Code` copies the current `RPM2` invite to the OS clipboard.
- `Join Clipboard` imports an invite from the OS clipboard and saves it as the local mesh identity.
- `New Group` generates a fresh device group and replaces the stored mesh identity.

When EasyTier mesh is enabled, saving a new group asks the unified networking runtime to refresh. In packaged macOS runs, the privileged daemon also sees the saved config change and restarts EasyTier with the new group automatically.

## Unified App Runtime Direction

The next product shape is one app with both roles:

- Idle/passive: the app advertises stream capability and can accept a remote-control session.
- Active/viewer: selecting a device starts a viewer session to that peer.
- Shared runtime: one mesh lifecycle, one discovery socket, one paired-device list, and one role state machine.

The migration path is now service-first. The old standalone `host` and `client` product binaries have been retired; their crates remain as internal libraries for capture/streaming and viewer/session services used by the unified app.

## Persistence And Secret Storage

Current code models, validates, persists, locates the sidecar binary, and can start/stop a sidecar process when explicitly used.

The platform-neutral store writes non-secret mesh metadata to `mesh.conf` and keeps `network_secret` out of that file. The secret goes through a `MeshSecretStore` abstraction:

- Current fallback: `AppPrivateMeshSecretStore`, which stores `mesh.secret` in the same app-private directory. On Unix, the directory is `0700` and the metadata/secret files are `0600`.
- Future macOS: implement `MeshSecretStore` with Keychain.
- Future Windows: implement `MeshSecretStore` with Credential Manager or DPAPI.
- Future Linux: implement `MeshSecretStore` with Secret Service/libsecret, with the app-private fallback only when a desktop secret service is unavailable.

`AppPrivateMeshConfigStore::load_or_generate` is the first-run path: it loads an existing mesh identity or generates and saves a new one automatically. The fallback config must still redact secrets in logs and UI diagnostics.

## Peer Discovery

EasyTier gives the virtual network reachability. RemotePlay still needs its own paired-peer discovery layer so the user sees devices instead of IP addresses. The planned path:

- Start EasyTier sidecar with RemotePlay's stable derived virtual IP.
- Advertise RemotePlay availability inside the mesh.
- Discover paired peers by device identity and virtual IP.
- Replace manual `REMOTE_PLAY_HOSTS` entries with paired devices once discovery is reliable.

LAN discovery can use the same discovery payload before or alongside EasyTier. The lightweight first version should use UDP broadcast on `38117` with a small `RPDISC1` packet containing:

- mesh `network_name`
- local `device_id`
- display name
- control port
- optional EasyTier virtual IP
- capability bits
- TTL

The packet never carries `network_secret` or invite material. Receivers should only show devices that match the local paired mesh identity, so random devices on the same LAN do not appear as trusted peers. This approach is very light and works well on the same subnet, but it can be blocked by guest Wi-Fi, VLANs, VPN routing, or OS firewall rules. mDNS/Bonjour can be added later as a nicer platform-native discovery adapter, but UDP broadcast is the simplest first slice and also works inside an EasyTier virtual network.

`remote_core::discovery::run_discovery_runtime` is the shared worker for this path. It binds a UDP socket, periodically sends the local `DiscoveryAnnouncement` to configured targets, listens for peer announcements, filters out other networks and the local device id, maintains a TTL-based peer cache, and publishes both events and snapshots. The first runtime test uses loopback unicast so it is deterministic; host/client integration can add LAN broadcast targets and EasyTier virtual-network targets behind a feature flag.

Host and client now start this worker behind:

```text
REMOTE_PLAY_DISCOVERY=1
```

The default UDP port is `38117`. Development builds can override it with:

```text
REMOTE_PLAY_DISCOVERY_PORT=<port>
```

Discovery sockets are opened with address reuse, and on Unix with port reuse, so separate host/client development processes can bind the same discovery port when the OS supports it. The unified dual-role app should still own a single socket later.

The host advertises itself as a stream-capable device on control port `39271` by default. Builds can override the bind address with `REMOTE_PLAY_HOST_BIND_ADDR`. The client advertises viewer capabilities; viewer-only announcements may use port `0`, and the Devices list filters those out so only stream-capable devices become connection rows.

When EasyTier is enabled and RemotePlay has an expected virtual IP, host/client include that virtual IP in the discovery announcement and mark the announcement as mesh scoped. A receiver then derives the RemotePlay control endpoint from the virtual IP instead of the packet source address, so LAN discovery can steer paired devices onto the EasyTier interface once it is ready. If the virtual IP is still pending, discovery falls back to normal LAN source-address behavior while the sidecar continues starting.
