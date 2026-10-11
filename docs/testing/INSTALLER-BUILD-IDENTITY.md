# Installer manifest and native backend identity integration

The installer task owns `scripts/install_macos_unified.py`, the new
`scripts/installer_build_identity.py`, installer regression fixtures and
`scripts/acceptance/launchd_background_review.py`. The five core gate files from
`bd242a4381abe63ac51589c27aab7c17e18b30d3` are an unchanged merged dependency.
The task also read the producer's `0f858a51773a5b5309291a78f6ef48f683255682`
embedding/interface changes without editing the shared checkout or its producer
files. Preserve both lifecycle entry fields and `build_identity` when combining
that producer commit with the lifecycle implementation.

## Independent handoff

The package must contain the exact signed `RemotePlay.app` inventory and one
external root `remoteplay-release-manifest.json` using the core gate's schema.
The installer receives these expectations independently of the package:

```sh
python3 scripts/install_macos_unified.py PACKAGE.zip \
  --separate-background \
  --expected-commit FULL_REVIEWED_COMMIT \
  --expected-identity-sha256 REVIEWED_BUILD_IDENTITY_SHA \
  --expected-manifest-sha256 SIGNER_HANDOFF_MANIFEST_SHA \
  --native-observer /absolute/canonical/reviewed-observer \
  --native-observer-sha256 REVIEWED_OBSERVER_SHA
```

The default remains a read-only plan. An authorized apply adds `--apply`.
All five expectation/observer arguments must be supplied together. Production
apply and background-role migration reject missing expectations. An installation
already bound to release identity also rejects a signature-only path. A legacy
read-only inspection reports `legacy_signature_only: true`,
`build_identity_preflight_verified: false`, and
`backend_running_image_verified: false`; absence is never a passed identity gate.
The existing scoped, disposable legacy acceptance fixtures remain explicitly
identified as such and cannot target production App/profile/service paths.

The producer must supply an observed-build receipt, sign with the existing
pinned identity, seal its final bytes and hand off the manifest digest. The
producer's `verified-build-candidate` and
`remoteplay-build-candidate-manifest.json` are not release receipts and cannot be
passed to `seal`. The installer never derives trusted expectations from the
candidate's own metadata or grants release authorization.

## Verification before and after migration

Before a stop or canonical-file mutation, the core gate streams and verifies the
entire original archive, its independent manifest anchor and complete file
inventory. The adapter then creates a bounded temporary App-only archive view
for the unchanged native signing guard. Only the previously verified external
manifest is omitted from that view. Payload bytes and Unix modes are preserved.
Original archive identity and the extracted App inventory are checked again;
the existing old/new signature comparison remains mandatory. No signer bypass,
replacement certificate or TCC change is introduced.

The gate's archive rules reject `__MACOSX` and AppleDouble extras. Existing ditto
fixture archives contain these extras, so production manifest ZIP generation
must use the exact App inventory plus the manifest. This adapter does not relax
that policy. Publication/stage integration remains with the producer's release
path and needs the same independently supplied expectations.

The exact executable reports ProductInfo only after its manifest hash is
verified. Its immutable `build_identity`, platform, architecture, version,
original GPUI and separated lifecycle entries must match the sealed record.
Staged and canonical App inventories/compiled metadata are rechecked. A sidecar
cannot retrofit missing identity into an old signed executable.

The backend adapter uses the actual loaded launchd job's executable, arguments
and PID, requires `--background-service`, checks the canonical plist label and
arguments, then binds the authenticated same-user lifecycle owner to that PID
and binary digest. The independently hashed native macOS observer verifies the
mapped executable vnode, start time and unchanged signed image. A final launchd
observation rejects a generation change during verification. Missing job
arguments, mismatched ownership, observer denial or an old mapped image fails.
No fabricated `--gui` argument is passed to the frontend API; frontend loaded
image, visual, media and interaction acceptance remain separate.

An unloaded existing service is a plan blocker. Identity-bound apply refuses to
start an unregistered service implicitly. Same-build metadata/role migrations
are also rejected; assign a new signed build for a changed release tuple.

## Transaction and recovery

The stable App's adjacent private
`Applications/.remoteplay-release-identity.json` stores the exact manifest bytes
(base64) and the independent expectations, outside the signed App and persistent
device profile. It contains public build identity, not membership secrets. The
installer makes private raw before/after backups; the bounded journal binds their
hashes and the canonical state path. Forward App/plist/role-marker/release-state
changes and restoration happen while holding the profile lock.

A known verification failure restores the signed previous App, exact original
plist, prior role marker and exact prior release state (including prior absence).
If the old release was identity-bound, its App/compiled identity and restarted
native backend are verified again. A legacy previous App retains signed rollback
protection but is explicitly not given a retroactive build-identity receipt.
Concurrent state edits, changed backups, terminal state mismatches and uncertain
launchctl outcomes refuse automatic replay. `--recover` restores the journaled
previous expectations and accepts no new expectation overrides.

## Evidence and remaining acceptance

The merged-dependency regression suite passed with **263 tests: 256 passed,
seven existing skips, zero failures**. Seventeen new installer identity tests
exercise real archive inventories and schema checks with explicitly synthetic
signer, product-metadata and native-process hooks. They verify read-only plans,
missing anchors/compiled identity, unverified observers, backend argument/PID
binding, successful state persistence, complete known-failure rollback,
interruption recovery, missing terminal state, and concurrent/tampered-state
refusal. Hook-based results are not actual product-build acceptance.

The independently labelled real signed launchd fixture passed **24 checks**,
including migration, normal GUI integration-hook exit, duplicate refusal,
background-only crash recovery, controlled-gap refusal and signed App/plist
failure rollback. The real reviewed native observer verified the backend image
after migration, recovery and rollback. A deliberately synthetic negative
manifest over the actual signed old App was refused specifically with
`Binary lacks the expected compiled source/build identity`, before stopping or
changing the running fixture App/plist. The initial negative fixture was rejected
earlier by AppleDouble archive layout; that failed assertion and receipt are
retained privately. All test jobs were removed.

The producer subsequently handed off the combined observed-build product from
`f6cbe7b627663185e0a0064901c8d2d1bd7b03cf`, signed build `20261010.1081`.
Its independently supplied handoff digest, observed compiler output/receipt,
source snapshot, compiled identity, signed bytes, archive and external manifest
were verified before any fixture operation. The five input expectations were
passed to the unchanged production installer, with no mocked validation hooks.

`scripts/acceptance/joint_identity_launchd_review.py` passed **24 native checks**:
identity-bound upgrade and exact manifest state persistence, signed older-build
and wrong-manifest rejection without stopping the backend, actual GUI render and
timed normal exit without respawn, backend-only launchd recovery, and actual
health failure with complete App/plist/release-state rollback and verified native
backend connectivity. Local-only signed builds `1080` and `1082` use the same
independently verified raw observed compiler output, receipt and pinned signer;
the installed current `1081` archive is the exact handed candidate. These build
variants are fixtures, not newly observed source compilations or published releases.

The health failure is a real SIGSTOP of the independently observed owned `1082`
backend after authenticated IPC proves it owns the profile lock. The production
health check fails, then production rollback restores the exact prior state.
launchd can terminate the stopped process before SIGCONT; the helper requires
either generation-verified SIGCONT or a kernel ESRCH existence check. The final
run observed real SIGSTOP and kernel-confirmed exit, not an assumed SIGCONT.
An observer error during bootout was retained and was not used as proof of exit.
The final restarted backend's mapped image, authenticated PID, source/build
identity and exact independently anchored `1081` manifest all passed again.

The first run stopped before upgrade because the fixture's socket path was
104 bytes, beyond Darwin's null-terminated address limit. The next run completed
real rollback but failed its overly strict SIGCONT assertion. Both failed receipts
were retained; their labels were removed. The final helper uses a shorter private
root, bounds socket length and explicitly verifies termination. The final label
was independently confirmed unloaded with no matching fixture process.

This closes the full native signed-product release-state transaction acceptance.
It does not extend the older hook-based tests into media or input acceptance.
Daily App/plist hashes and the shared checkout's original changes are preserved.
Manual close-button CUA approval remains unanswered; no new card/tool bypass,
real login/reboot, physical two-device acceptance or production deployment ran.
Raw labels, PIDs, ports, paths, binaries and receipts remain local. Push remains
paused after the earlier automatic approval rejection; there was no alternate
publication path or retry.
