# Weavelo relay instance map — 2026-10-22

This is a bounded read-only observation, not a deployment or pairing receipt.
No service, client, profile, identity, permission or network setting was changed.
No invitation, group capability or secret-derived digest was exported.

## Deployed relay

The existing authorized `home-nas` route reports:

| Item | Actual observation |
| --- | --- |
| Unit | `remote-play-relay.service`, active/running, PID 11384 |
| Executable | `/opt/remote-play-relay/remote-play-relay` |
| SHA-256 | `67cbd4a1911d8f44574061745deedbcc6262955e6cc1d9124bfcda8b1ee3c20e` |
| Transport | WebSocket enabled; loopback `127.0.0.1:39491` listening |
| Public route | `wss://relay.hackerlife.fun:8443/v1/relay` |
| Actual Caddy route | listener `:8443`, `/v1/relay` → `127.0.0.1:39491` |
| Connections | Eight established TCP connections at observation time; not attributed to devices or groups |
| New instance | `weavelo-relay.service` absent; new executable absent; no Weavelo-named Caddy host |
| Candidate separate port | 39493 was not listening; this is not a reservation or approval |

The binary contains the `RPR1` wire marker, also used by the client sources.
This does not identify its exact build commit or prove every protocol feature.
The relay executable is environment-driven: running it with `--version` or
`--help` is not a safe metadata query and was not attempted. Existing journal
access returned one row and no registration, decode-failure or forwarding-error
rows. The deployed service offers no existing live per-group registry query;
zero observed log errors is not proof that a particular peer registered.

The NAS reads finished successfully with exit 0:
`01a1636f-37ed-7e22-90f3-8cde455cbd53` (unit/Caddy map) and
`01a16372-cbea-7be1-a493-0440c36b223c` (retained unit/binary/listeners).
They are independent read-only operations, not a replay of any prior transaction.

## Client bindings

| Device | Retained binding | Independent Desktop profile |
| --- | --- | --- |
| Studio | `fun.hackerlife.weavelo.host`; `~/Applications/Weavelo.app`; `~/Library/Application Support/Weavelo/NativeMesh` | `~/Library/Application Support/Weavelo/Desktop` absent |
| HO5 | active `weavelo.service`, PID 1397692; `/var/home/liang/.local/opt/weavelo/weavelo`; `/var/home/liang/.config/weavelo/mesh` | `~/.config/weavelo/desktop` absent |
| cube | PID 34140, `%LOCALAPPDATA%/RemotePlay/remote_play.exe`; retained `%APPDATA%/RemotePlay/Mesh` exists | `%APPDATA%/Weavelo/Desktop` absent |

HO5 unit/profile observation `4fee61d7-91dd-4f68-9358-5dfbc9a51902`
exited 0. cube observation `e152e765-48ad-45c8-8682-b40e8f903a6b`
returned the rows above but exited 1 because the optional `weavelo` process-name
lookup found no process; it is not a successful whole-command receipt. No old
unsupported cube/Studio diagnostic command was repeated. Book is under another
owner's installation/signing work and was not queried again here.

The retained source commits `db9fc29`, `d5ecef1` and `cd5dc905` all declare the
same public WebSocket URL above. Selected launch-unit environment fields contain
no relay override on Studio/HO5. This does not independently prove a running
process has no inherited override or that old DNS results reach the same backend.
Earlier safe endpoint observations found Studio/cube's numeric targets differed
from HO5's; no reconnect, DNS setting change or fresh old-client handshake was
forced to resolve that historical difference.

## What HO5 receive=0 establishes

The latest existing actual GUI diagnostic, operation
`15ad17e9-b44a-49a2-9e30-89e2484c4434`, is from installed source `d5ecef1`:
both relay channels connected/active, HTTP 101, registrations sent and matching
Pongs received; relay discovery data received/decoded/accepted all zero.
UDP decoded only its own announcements; owner and GUI projections had zero routes.
Thus it is not an unconfigured relay, a total connection failure, a GUI filter,
or a received packet being rejected by the discovery decoder.

The prior authorized comparison established that HO5's retained group differs
from Studio/cube's. No JoinGroup operation has subsequently executed. The relay
forwards only within the same derived channel/group, and never echoes data to
the same connection. This is a concrete isolation blocker consistent with
receive=0, not evidence of a forwarding code defect. Control uses
`control-device-v1`; discovery uses `discovery`, deliberately distinct namespaces.
The new Desktop instance has not joined any group because its profiles do not
yet exist. No current server-side group registry was available to establish that
group isolation is the only blocker. The wire marker and client keepalives alone
do not constitute full compatibility or real device-to-device acceptance.

## Concrete next scope requiring confirmation

The independent delivery can use the existing authorized NAS, without replacing
its retained service:

1. Approve a dedicated Weavelo public hostname. The concrete proposal is
   `weavelo.hackerlife.fun:8443`, path `/v1/relay`; it is not selected, provisioned
   or known available by this observation. This is separate from Octendra's host.
2. Install the verified new relay artifact as
   `/opt/weavelo-relay/weavelo-relay`, unit `weavelo-relay.service`, using a
   separate loopback port, proposed `127.0.0.1:39493` (recheck before apply).
   Add only the approved hostname's DNS/TLS/reverse-proxy mapping. Preserve
   `relay.hackerlife.fun`, port 39491 and the old unit unchanged. NAS receives
   artifacts only; it does not build source.
3. Configure only the independent clients' new launchers/profiles with
   `WEAVELO_RELAY=1` and
   `WEAVELO_RELAY_SERVER_ADDR=wss://<approved-host>:8443/v1/relay`.
   Keep control/discovery ports 39273/38119 and all old profiles intact.
4. Create one new group, proposed display name `Weavelo Desktop`, through the
   original invitation flow. Join the new Studio, HO5, cube and Book profiles,
   with Book coordinated through its existing owner. This grants these new
   instances membership; it does not copy their retained device identities or
   replace their old groups. Secrets must stay in the original private UI or an
   already authorized protected input path, never tool text or logs. Phone is excluded.
5. Verify actual cross-device discovery, both directions of push/pull, and then
   the separately authorized temporary-window computer-use session. Connection
   101, namespace equality or synthetic relay traffic cannot replace this.

Steps 1–4 introduce a new persistent endpoint/group and need confirmation of
that concrete scope. Signing, native package and platform execution restrictions
remain separate dependencies; this observation does not waive them. No new
client was installed and no real device discovery, transfer or input success is
claimed. Source `96eaad4` remains distinct from the older prepared macOS package
(`f3708dd`) and the native Windows/Linux binaries (`dd66581`).
