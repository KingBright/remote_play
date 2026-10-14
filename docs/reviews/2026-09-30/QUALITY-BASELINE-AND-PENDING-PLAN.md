# Quality baseline and pending major-change plan

The user's four simultaneous requirements are recorded in docs/plans/GUI-RESTORATION-QUALITY-CONTRACT.md. Major restoration/native-media/default-entry/deployment work is not approved by this report. No GUI or media runtime was switched this iteration.

The feature inventory is the union of original product UI, current safety/functional work and mobile/native services, not the simplified alpha.7 UI alone. The first catalog covers 31 feature groups with 58 checked source paths. Source presence is not behavior acceptance; all migration acceptance entries remain pending and known platform/Unicode/native-performance gaps remain explicit.

239 source/configuration/documentation files were snapshotted locally before changes with per-file SHA-256. This is source recovery, not a complete binary/asset/machine backup. The source archive and manifest are in /Users/jinliang/Workspace/.rp-quality-baseline-20260930.

Actual code work: four new cfg(test) lifecycle tests were added around LatestFrameSlot, without modifying its production implementation. Their remote run was blocked before execution and no alternative route was used. They are not marked passed. The standalone comparable-report checker scripts/compare_performance.py passed 18 synthetic-data tests, including resolution/route/measurement boundary mismatches, lower fps, new copies from a zero baseline, NaN, missing quality evidence and missing metrics. It only compares supplied reports, does not collect physical performance, and is not wired into or authorized to change production publishing.

Installed-state observations remained alpha.7 on both Macs and HO5; Windows installed alpha.7 was stopped, UU remained Running. No app processes were restarted or capture/input launched this iteration. No permissions, membership, signatures, autostart or NAS publication were changed. No commit/push was performed.

Final source hash comparison confirms that among the snapshot files only AGENTS.md and test-only additions to latest_frame.rs changed; production Rust body remained identical. New files are the comparator, its tests, feature catalog and proposed contract. Detailed machine-readable record: /Users/jinliang/Workspace/.rp-quality-baseline-20260930/iteration-receipt.json.

Pending user decision: continue original GPUI/yororen product restoration on the current shared safe Rust model, independently finish native video presentation per platform, keep Android's existing GUI/media path, do not move to Flutter or remove functionality without renewed confirmation, and switch each production default only after agreed feature/visual/input/performance acceptance. Until confirmation, continue only small nonbehavioral regression/measurement preparation and already authorized fixes.
