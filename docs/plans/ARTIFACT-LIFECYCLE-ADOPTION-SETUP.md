# Local lifecycle metadata

The tracked template is a portable starting point, not a runtime authority. The existing local ARTIFACT-LIFECYCLE-ADOPTION.json remains the sole authority used by scripts/check_artifact_lifecycle.py. Its adoption binding remains at .rust-storage/adoption.json. Both local files are preserved on disk and excluded from Git because they contain machine-specific storage paths and ownership identifiers.

For a new checkout, copy ARTIFACT-LIFECYCLE-ADOPTION.template.json to ARTIFACT-LIFECYCLE-ADOPTION.json, replace both shared-target placeholders with one reviewed existing absolute target path, and review the ownership/protection fields. Use the installed rust-project-lifecycle helper adopt command with that authority, project and exact artifact reference to create the local adoption binding. Run python3 -B scripts/check_artifact_lifecycle.py check and dry-run. The placeholders intentionally fail validation until reviewed.

Keep an existing machine authority, binding, delivery registry and interrupted transition journals intact. Do not copy another machine identifiers or sign/install/update a product as part of adoption. Release-only setup, when needed, must use the reviewed local authority and exact existing output roots; it does not import or validate current products. Source, lockfiles, tests and this template stay tracked.

Migration verification on the destination preserves the shared Cargo target, false coverage flags and protected legacy references. Local check success means observation metadata is consistent; it does not establish release, signing, installation or visual acceptance.
