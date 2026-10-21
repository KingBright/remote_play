# Independent Weavelo desktop delivery

The retained clients and servers remain maintenance channels. The new desktop
identity is `fun.hackerlife.weavelo.desktop`, with display name **Weavelo** and
the original signing certificate. The public contract lives in
`deploy/desktop/product-identity.json`; it is embedded in the actual executable's
`--product-info-json` output.

On macOS, the new installer targets `~/Applications/Weavelo/Weavelo.app`, profile
`~/Library/Application Support/Weavelo/Desktop`, and launch agent
`fun.hackerlife.weavelo.desktop.host`. It never imports or rewrites the retained
`Weavelo.app`, `NativeMesh`, `RemotePlay` profile, or old launch agents. The IPC
protocol and socket basenames are unchanged; their complete paths and instance
locks belong to the separate profile. Windows defaults use `Weavelo/Desktop`;
Linux defaults use `weavelo/desktop` and supervisor `weavelo-desktop.service`.

New local ports are control 39273 and discovery 38119. There is no implicit
registration through the retained relay or rendezvous. Configure the new service
explicitly through the existing `WEAVELO_RELAY_SERVER_ADDR` / relay and P2P
settings once its endpoint is known. No new network grant, locator or credential
is installed by this preparation flow. In particular, existing local Octendra
authorization does not automatically authorize the new profile. Its approved
Weavelo route must bind that profile, current GUI generation and exact target.

## Package preparation and signing

First build from the primary main checkout with the existing
`build_identity_gate.py` observed-build entry. Supply its independently observed
six `WEAVELO_BUILD_RECEIPT`, `WEAVELO_BUILD_RECEIPT_SHA256`,
`WEAVELO_EXPECTED_COMMIT`, `WEAVELO_EXPECTED_IDENTITY_SHA256`,
`WEAVELO_PREBUILT_BINARY`, and `WEAVELO_PREBUILT_SHA256` handoff values.

`bash scripts/package_macos_unified.sh --prepare-desktop` prepares the actual
bundle through the original producer lock and public output ledger. Its fixed
current component is `macos-desktop-arm64-preparation`, archive
`~/rust-target/package/macos/Weavelo-desktop-macos-arm64.prepared.zip`.
This explicit unsigned preparation is **not installable** and is recorded as
`unsigned-dev`; it has no signed-release receipt. Both signed-archive consumers
reject its preparation marker. No signing failure silently selects this mode.

Once the existing pinned identity is available, the same formal entry with
`--desktop` signs and verifies a distinct
`Weavelo-desktop-macos-arm64.zip` component under the same public authority.
The original release component and installed maintenance app are untouched.

## Independent installation

`python3 -B scripts/install_macos_desktop.py` shows the collision-checked plan.
To apply a genuinely signed archive, provide `--apply` and all four independent
`--expected-commit`, `--expected-identity-sha256`, `--expected-manifest-sha256`,
and `--expected-archive-sha256` pins. Original signature and source/manifest gates
run before destination writes. The native move uses exclusive rename; an
existing target, profile, service file or journal blocks replay. `--status`
reads the original new-install journal after a partial or uncertain result.

Installation creates only the new app, empty private profile and new launch-agent
file. It does not bootstrap a service, start a stream, import pairing, reuse old
authority or change OS permissions. There is no additional legacy rollback
engineering. Native capture/input acceptance requires the signed independent app,
its own existing authorization and an explicitly selected temporary test window.

## Focused verification

`python3 -B -m unittest scripts.tests.test_desktop_parallel_delivery` checks old
app/profile/service preservation, destination and symlink collisions, replay
rejection, original signer pin, bundle layout, prepared-package rejection and a
native exclusive-move fixture. That fixture stubs signature/compiled-info gates;
it proves filesystem behavior, not a signed release or real input.

`python3 -B scripts/test_stream_settings_model.py --mode desktop-identity` runs
the Rust namespace/path tests through the fixed source/process guard. The actual
Studio mode uses the existing native release configuration and shared cache;
the JSON identity input is included in its before/after source guard. The actual
product build also exports and verifies runtime default ports and disabled
implicit relay/P2P before its bundle can be prepared.
