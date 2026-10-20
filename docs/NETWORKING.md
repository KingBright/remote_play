Weavelo networking is product-native. The route policy is **LAN direct → Weavelo P2P direct → Weavelo Relay**. No third-party overlay network or sidecar is required.

# Weavelo Networking

## Product Route

Weavelo owns the complete connectivity path. The runtime selects one route per remote device in this order:

1. **LAN direct** — use local discovery and send Weavelo UDP datagrams straight to the peer.
2. **Weavelo P2P direct** — use the `bw` rendezvous node only to exchange observed UDP endpoints and coordinate hole punching, then send Weavelo datagrams peer-to-peer.
3. **Weavelo Relay** — if no direct path is usable, use the NAS WebSocket relay as the final fallback.

The selected route is represented by the same local Weavelo UDP endpoint, so capture, video/audio packetization, input, clipboard and file-transfer code do not need separate transport implementations.

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

Weavelo discovery uses the compact `RPDISC1` UDP announcement on port `38117` by default. Announcements contain device id, display name, capabilities and control port, but never the private group secret.

The discovery cache keeps multiple route candidates for the same device. The unified app ranks them:

```text
LAN < P2P direct < Relay
```


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

It does **not** proxy video, audio, input or file data. Once `Punch` / `PunchAck` or real peer traffic confirms the path, Weavelo publishes a `DiscoveryScope::P2p` candidate locally. Candidate discovery alone is not enough to select P2P.

One public UDP socket is shared by the local app, while each remote peer receives an independent loopback route socket. This lets one Weavelo instance maintain direct paths to several devices without one candidate overwriting another.

Server deployment lives under `deploy/bw/`.

## NAS Relay

Public endpoint:

```text
wss://relay.hackerlife.fun:8443/v1/relay
```

The NAS relay is the last fallback only. The client opens local control/discovery tunnel endpoints; existing Weavelo UDP packets are wrapped in the small `RPR1` relay envelope and forwarded over WebSocket. Caddy terminates TLS and proxies `/v1/relay` to the Rust relay service.

Deployment files live under `deploy/nas/`.

## Runtime Configuration


```text
WEAVELO_P2P=1
WEAVELO_P2P_RENDEZVOUS=p.hackerlife.fun:3478
WEAVELO_RELAY=1
WEAVELO_RELAY_SERVER_ADDR=wss://relay.hackerlife.fun:8443/v1/relay
WEAVELO_DISCOVERY=1
```

Useful development overrides:

```text
WEAVELO_P2P_BIND_ADDR=0.0.0.0:0
WEAVELO_P2P_LOG=1
WEAVELO_RELAY_LOG=1
WEAVELO_DISCOVERY_PORT=38117
```


## Performance Rules

- LAN and P2P paths carry native Weavelo UDP datagrams without an extra media serialization layer.
- Rendezvous never enters the media hot path.
- Stale P2P candidates are not promoted to the device list.
- Relay is a reachability fallback, not a preferred route.
- Realtime media must never queue behind file/clipboard bulk traffic.
- Route changes should preserve the same protocol/session model so recovery does not rebuild the entire streaming stack.
