# RemotePlay Networking

## Product Route

RemotePlay owns the complete connectivity path. The runtime selects one route per remote device in this order:

1. **LAN direct** — use local discovery and send RemotePlay UDP datagrams straight to the peer.
2. **RemotePlay P2P direct** — use the `bw` rendezvous node only to exchange observed UDP endpoints and coordinate hole punching, then send RemotePlay datagrams peer-to-peer.
3. **RemotePlay Relay** — if no direct path is usable, use the NAS WebSocket relay as the final fallback.

The selected route is represented by the same local RemotePlay UDP endpoint, so capture, video/audio packetization, input, clipboard and file-transfer code do not need separate transport implementations.

```text
same LAN
A  <------------------------------>  B

cross-network, P2P succeeds
A  ---- register ----> bw:3478 <---- register ----  B
A  <=============== direct UDP =================>  B
                 bw leaves the media path

P2P unavailable
A  <------ WSS ------> NAS relay <------ WSS ------> B
```

Third-party streaming/network overlays are not part of the default product route.

## Device Group Identity

A device group contains a random group name, secret and per-install device id. The existing `RPM2` invite code remains the pairing payload.

The private group secret is never sent to public infrastructure. Separate opaque capabilities are derived locally with HMAC-SHA256 for P2P rendezvous and relay namespaces. This prevents the public services from learning the device-group secret while keeping peers isolated by group.

Changing device groups reloads P2P, relay and discovery together so no component remains attached to the previous group.

## LAN Discovery

RemotePlay discovery uses the compact `RPDISC1` UDP announcement on port `38117` by default. Announcements contain device id, display name, capabilities and control port, but never the private group secret.

The discovery cache keeps multiple route candidates for the same device. The unified app ranks them:

```text
LAN < P2P direct < legacy mesh < Relay
```

The legacy-mesh rank exists only for old/manual builds. EasyTier is disabled by default and is no longer bundled into the macOS product package.

## P2P Rendezvous on bw

Public endpoint:

```text
p.hackerlife.fun:3478/udp
```

`remote_core::p2p` owns the wire protocol and hole-punch runtime. The rendezvous server stores only short-lived in-memory registrations:

- opaque group capability
- peer id
- observed UDP socket address
- discovery announcement
- last-seen timestamp

It does **not** proxy video, audio, input or file data. Once `Punch` / `PunchAck` or real peer traffic confirms the path, RemotePlay publishes a `DiscoveryScope::P2p` candidate locally. Candidate discovery alone is not enough to select P2P.

One public UDP socket is shared by the local app, while each remote peer receives an independent loopback route socket. This lets one RemotePlay instance maintain direct paths to several devices without one candidate overwriting another.

Server deployment lives under `deploy/bw/`.

## NAS Relay

Public endpoint:

```text
wss://relay.hackerlife.fun:8443/v1/relay
```

The NAS relay is the last fallback only. The client opens local control/discovery tunnel endpoints; existing RemotePlay UDP packets are wrapped in the small `RPR1` relay envelope and forwarded over WebSocket. Caddy terminates TLS and proxies `/v1/relay` to the Rust relay service.

Deployment files live under `deploy/nas/`.

## Runtime Configuration

Defaults are product-safe and require no EasyTier installation:

```text
REMOTE_PLAY_P2P=1
REMOTE_PLAY_P2P_RENDEZVOUS=p.hackerlife.fun:3478
REMOTE_PLAY_RELAY=1
REMOTE_PLAY_RELAY_SERVER_ADDR=wss://relay.hackerlife.fun:8443/v1/relay
REMOTE_PLAY_DISCOVERY=1
```

Useful development overrides:

```text
REMOTE_PLAY_P2P_BIND_ADDR=0.0.0.0:0
REMOTE_PLAY_P2P_LOG=1
REMOTE_PLAY_RELAY_LOG=1
REMOTE_PLAY_DISCOVERY_PORT=38117
```

Set `REMOTE_PLAY_MESH=1` only for deliberate legacy EasyTier testing. It is off by default and not included in normal packaging.

## Performance Rules

- LAN and P2P paths carry native RemotePlay UDP datagrams without an extra media serialization layer.
- Rendezvous never enters the media hot path.
- Stale P2P candidates are not promoted to the device list.
- Relay is a reachability fallback, not a preferred route.
- Realtime media must never queue behind file/clipboard bulk traffic.
- Route changes should preserve the same protocol/session model so recovery does not rebuild the entire streaming stack.
