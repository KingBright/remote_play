# NAS Relay Deployment

RemotePlay uses a WebSocket relay on the Synology NAS so the existing Caddy listener on external port `8443` can serve both HTTP applications and RemotePlay. Direct LAN and RemotePlay P2P routes remain preferred; this relay is the final restricted-network fallback.

## Public Route

- DNS: `relay.hackerlife.fun` is a DNS-only CNAME to `www.hackerlife.fun`.
- Public endpoint: `wss://relay.hackerlife.fun:8443/v1/relay`.
- Caddy upstream: `127.0.0.1:39491`.
- Service binary: `/opt/remote-play-relay/remote-play-relay`.
- Service unit: `/etc/systemd/system/remote-play-relay.service`.

The relay namespace is derived locally with HMAC-SHA256 from the paired device group's network name, network secret, and channel name. The group secret is never sent to or stored on the relay.

## Build

From an Apple Silicon development Mac with Zig installed:

```bash
env -u ANTIGRAVITY_SOURCE_METADATA \
  cargo zigbuild --offline -p remote_core --example tcp_relay_server \
  --release --target x86_64-unknown-linux-musl
```

This workspace may configure a shared Cargo target directory. Use `cargo metadata --no-deps --format-version 1` to locate the output instead of assuming it is under the repository's `target/` directory.

## Apply Order

1. Record the current FreshLoop health, Caddy configuration digest, listeners, and service states.
2. Back up `/etc/caddy/Caddyfile` and any existing relay service/unit files with one deployment timestamp.
3. Upload the SHA-256-pinned static relay binary and service unit.
4. Start the relay and verify `127.0.0.1:39491` before changing Caddy.
5. Append `Caddyfile.remote-play`, validate with `caddy validate`, and reload Caddy.
6. Create the DNS-only Cloudflare record.
7. Verify local upstream, Caddy health, public DNS/TLS/WSS, and FreshLoop regression checks.

## Rollback

1. Disable and stop `remote-play-relay.service`.
2. Restore the timestamped Caddyfile backup.
3. Validate and reload Caddy.
4. Remove the RemotePlay DNS record if the public route should be withdrawn.
5. Verify `news.hackerlife.fun:8443`, `nexus.service`, and the Caddy `8443` listener again.

Never store the Cloudflare token in this repository or directly in the Caddyfile. Keep it in a root-readable environment file referenced by `caddy.service`, and rotate any token that has appeared in logs or command output.
