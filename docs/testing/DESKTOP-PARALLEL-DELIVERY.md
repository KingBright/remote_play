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

## Linux independent archive and first install

`bash scripts/package_linux_desktop.sh` reuses an already observed native Linux
x64 binary; it never invokes Cargo build. Its required arguments are `--binary`,
`--binary-sha256`, `--build-receipt`, `--build-receipt-sha256`,
`--expected-commit` and `--expected-identity-sha256`. The original producer lock
and public current-output authority own `target/package/linux/Weavelo-desktop-linux-x64.zip`
and its `.verification.json` companion. The sealed manifest retains the binary's
original source commit; the companion separately records packaging source.
This Linux package has verified source/hash provenance, not a signing claim.
The installed public helper's original shared cleanup exclusion is mandatory.
An unsupported home alias or missing `shlock` blocks packaging; this entry does
not change that lock, modify the helper, or select an alternate output authority.

The archive includes the binary, original manifest, installer and its small
standard-library verification dependencies, identity, launcher/service templates
and icon. It contains no profile, credentials, user configuration or shared
libraries. Runtime system dependencies must already be present on the target.

The read-only installation plan requires Python 3.11 or newer:

```sh
python3 -B scripts/install_linux_independent.py PACKAGE.zip \
  --expected-archive-sha256 ARCHIVE_SHA256 --expected-manifest-sha256 MANIFEST_SHA256 \
  --expected-commit BINARY_COMMIT --expected-identity-sha256 BINARY_IDENTITY_SHA256
```

Use the four independently obtained values from the verified delivery handoff.
The same entry is included under `Weavelo/scripts/` for offline use after checking
the archive SHA256 and extracting it into a user-controlled directory. Default
plan performs no destination writes. Actual first install additionally requires
`--apply --expected-plan-sha256 PLAN_SHA256`; this round does not apply it on a
real user home. Every destination must be absent. Any existing app, profile,
launcher, service, desktop entry, icon, staging directory or journal rejects
replay. After a partial result inspect `--status` rather than rerunning apply.

Fixed new targets are `~/.local/opt/weavelo/desktop/`, empty private
`~/.config/weavelo/desktop/`, launcher `~/.local/bin/weavelo-desktop`, service
`~/.config/systemd/user/weavelo-desktop.service`, and desktop/icon ID
`fun.hackerlife.weavelo.desktop`. The installed launcher ignores inherited XDG
profile selection and forces the new profile, ports 39273/38119, relay off and
P2P off. The unit uses the same profile and ports. No systemctl, desktop cache,
network, capture/input permission or pairing action runs during installation.
The retained client and its group remain untouched; new endpoints and normal
invitation/authorization must be configured separately before connecting.

`python3 -B -m unittest scripts.tests.test_linux_independent_package` runs bounded
filesystem/package fixtures. Native Linux additionally exercises no-replace
installation in a temporary home and replay rejection; its synthetic binary is
never real GUI/input acceptance. Real package verification executes only the
already pinned binary's no-window ProductInfo path during packaging.
