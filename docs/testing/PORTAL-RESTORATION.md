# Linux Portal restoration

The RP host already admits a paired, authenticated connection without an
application Allow dialog. HO5's earlier bounded attempts called system
ScreenCast/RemoteDesktop selection and never obtained a successful choice;
they do not prove that a dialog was visible. A read-only query on 2026-10-12
confirmed ScreenCast v5 and RemoteDesktop v2, with keyboard/pointer capability.

The Linux share owner now requests application-lifetime persistence (mode 1)
and uses the system's single-use restore token for the same verified target and
share kind. The token stays in memory, retains its bus connection, is consumed
before awaiting a restore, and is replaced only after capture preparation and
the authenticated host commit succeed. It is neither logged nor serialized.
Choose another source opens a fresh system picker while retaining the old
source until commit. Stop, cancellation, revocation or changed authentication clears
the reusable state; stale rows are rechecked before calling the system portal.
Combined capture/control uses persistence only on RemoteDesktop. Version 1 or
a partial input grant cannot produce a remembered control grant.

This does not create permanent permission (mode 2), suppress a system-required
dialog, or restore across RP restarts or a new authentication nonce. The current
wire protocol authenticates paired-network membership, not a stable per-device
identity; an address or display name must not redirect a previous peer's grant.
Cross-connection unattended restoration remains a separate integration gap.
The first system choice and actual HO5 restoration still need native acceptance.

Tests use a private D-Bus service, plus the actual application share-owner source
with only the native PipeWire worker mocked. Run the latter alone with
`cargo test -p host --lib production_share_owner_restores_stops_and_rejects_changed_authentication -- --ignored --test-threads=1`.
Its synthetic effects are not a system authorization or deployment receipt.

[RemoteDesktop persistence](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
and [ScreenCast persistence](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
define the mode, single-use renewal and possible normal confirmation fallback.
