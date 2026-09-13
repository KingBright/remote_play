# bw P2P Rendezvous

`bw` is RemotePlay's lightweight UDP rendezvous node. It is not a media relay and does not carry video/audio after a direct path is established.

- Public endpoint: `p.hackerlife.fun:3478/udp`
- Service: `remote-play-p2p-rendezvous.service`
- Binary: `/opt/remote-play-p2p/remote-play-p2p-rendezvous`
- Default product route: `LAN -> P2P direct via bw rendezvous -> NAS WebSocket relay`

The rendezvous service only exchanges observed UDP endpoints between peers that present the same opaque HMAC-derived device-group capability. The device-group secret itself is never sent to `bw`.

The service is intentionally bounded: 16 peers per group and 4096 live registrations globally by default. Registrations expire quickly, so the server holds no durable peer database.

## Build

From the macOS development machine with `cargo-zigbuild` and Zig installed:

```bash
cargo zigbuild --release -p remote_core --example p2p_rendezvous_server \
  --target x86_64-unknown-linux-musl
```

## Deployment

Copy the static binary to `/opt/remote-play-p2p/remote-play-p2p-rendezvous`, install the supplied systemd unit, then enable it. Verify public UDP from a different network before treating P2P as healthy.

Do not place media forwarding logic on this service. If NAT traversal fails, the client must fall through to the NAS relay instead.
