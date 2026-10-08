# RemotePlay vendor patches

## screencapturekit 1.5.4: unused eframe dev dependency

RemotePlay uses this vendored crate for its ScreenCaptureKit library API. The normalized Cargo.toml had an eframe 0.33 dev-dependency, but the vendored package disables automatic examples/tests, declares no eframe example target, has no egui viewer source, and its Rust source/tests do not reference eframe or egui. Removing this orphan dev-dependency also removes eframe/egui and packages reachable only through that edge from this package's private Cargo.lock. Standard Cargo metadata resolution with the crates.io registry produced a canonical 449-package lock with no eframe/egui entries; that exact lock passed cargo metadata --locked and is copied into the vendor tree.

This patch does not change ScreenCaptureKit library code, public API, runtime dependencies, other examples, LICENSE/NOTICE files, or upstream documentation. The first offline resolution lacked the local bevy_hierarchy index entry; after allowing Cargo to update the standard crates.io index, metadata resolution and --locked verification succeeded. No compile was attempted. Cargo.toml.orig, README.md, examples/README.md, and CHANGELOG.md remain as upstream provenance; their old viewer references are historical and do not register a Cargo target.

The exact pre-patch normalized manifest and private lock are recoverable from local Git stash 9f756495bc8b8c6de84c051a2c4c101fe91d1848, third parent. Restore either file with:

- git show 9f756495bc8b8c6de84c051a2c4c101fe91d1848^3:third_party/screencapturekit/Cargo.toml > third_party/screencapturekit/Cargo.toml
- git show 9f756495bc8b8c6de84c051a2c4c101fe91d1848^3:third_party/screencapturekit/Cargo.lock > third_party/screencapturekit/Cargo.lock
