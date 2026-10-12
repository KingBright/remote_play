# Repeatable desktop stages

`python3 -B scripts/remoteplay_ci.py STEP --spec SPEC.json --journal PRIVATE_RUN.json`
is a local checked entry over the existing Rust storage, package, identity,
installer/recovery and NAS publication tools. It prints a dry-run by default;
`--apply` executes that one stage within existing authorization. It does not
grant device, signing, deployment or OS permission.

The spec pins schema1, actual machine hostname and architecture, execution role,
repository absolute path and full Git source commit, target platform, and the
absolute artifact path/SHA256/purpose. Compile/test/package additionally require
the independently reviewed `source_snapshot_sha256` from existing
`build_identity_gate.py inspect-source`; dirty user contents are included in that
comparison and never reset. Builds use only the project's existing owned cache
entry. Observe-only/shared legacy projects must retain their existing owner
workflow rather than receive an invented target override.

Stages: `build`, `test`, `package`, `deploy-plan`, `deploy`, `verify`, `rollback`,
`publish-plan`, `publish`. Native build/test accept the existing RemotePlay
package/features or the guarded diagnostic example. Packaging/installation are
currently wired to the existing macOS tools; non-macOS platform owners remain
required. Product package/deploy verification carries source commit, build
identity SHA, manifest SHA and native observer SHA into existing gates. A
diagnostic helper cannot be installed or published as a product.

The journal records a stage before execution and holds an exclusive run lock.
The same checked spec/step reuses a completed receipt without running again.
Failed, running or unknown outcomes stop; inspect the original external
operation or installer journal before explicit recovery. A completed receipt is
historical, not proof of the current remote machine; perform its verification
stage separately. The wrapper never rolls back a Git tree or unrelated user
contents. Logs/specs contain no credentials or private signing keys.

`publish_desktop_nas.sh STAGE --dry-run` performs local signature/byte preflight
and prints the artifact plan. `--verify-only` skips upload/remote writes and
checks public downloads. Normal publication preserves immutable filenames;
existing artifact and index bytes are not rewritten when identical. NAS only
receives runtime packages and publication metadata: no repository checkout,
build cache, compiler, Cargo, Gradle or Docker build runs on NAS.

Actual 2026-10-12 validation: 16 temporary-fixture workflow/receiver tests passed;
the real guarded helper SHA/platform check ran through `verify --apply` and its
repeat was a no-op. Signing, installation, recovery, NAS publication and live
Mac-to-Mac input were not executed by that check. Original helper transfer
`9532b631-9b71-47bf-b7d5-5ad572cf1fe5` is paused at source-address policy with zero
confirmed bytes; resume only that original operation after normal source routing
and source authorization are restored. Do not relabel or reroute it.
