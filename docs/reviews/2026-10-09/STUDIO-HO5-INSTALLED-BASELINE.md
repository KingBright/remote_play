# Mac Studio / HO5 installed alpha.8 baseline, 2026-10-09

This run exercised the existing installed original GPUI applications on both devices. It does **not** validate the prepared `20261009.181822` package, the new Ely/MVVM implementation, all twelve platform directions, or full visual acceptance. No production build, package, installation, publication or passed offline suite was repeated.

## Actual installed programs

Mac Studio is alpha.8 / build `20261004.80`, signed executable SHA-256 `bfb517967a3171c9ce1b3e6814e8a0167470b7a7cc4a795a9ab7e2dec2e67f5b`, at the canonical user app path. HO5 runs its installed alpha.8 artifact, SHA-256 `4cd160cef5da7644486e7f46a695ad9753e8994820986c00723334a786adac2c`, through the existing user service. Its public manifest records source ID `67bf20c6295b898e95c803c7898ec7f5e547bb162bae6d3d7c90fe42dfd37c33`. HO5's running executable could not be independently hashed through /proc; the installed file, service path, process and live GUI receipts are separate evidence.

Both actual installed binaries returned exit 0 for `--product-info-json`, reporting original GPUI as the compiled default and native video enabled. This corrects the earlier unsupported suspicion that the installed programs lacked that command.

## Real attempts

Direction below is **producer → viewer**. Every recorded connection used Relay.

| Attempt | Actual source | Duration | Decoded frames | Decode errors | GPUI scene frame | Result boundary |
| --- | --- | ---: | ---: | ---: | --- | --- |
| Studio → HO5, product-window request | Requested Window 13773; request did not select it | 45 s | 0 | 0 | false | Responsive connection only |
| Studio → HO5, owned native window A | Window 14299, PID 51140, exact title | 30 s | 749 | 0 | true | Native decode and GPU submission |
| Studio → HO5, reconnect to native window B | Window 14300, same fixture PID, exact title | 30 s | 737 | 0 | true | New connection and source binding |
| HO5 → Studio | MainDisplay selection could not be driven | 30 s | 0 | 0 | false | Responsive connection only |

The first attempt was retained, including its zero-frame result. The bounded test hook requires exact source ID, process ID and title before requesting a window stream. Its failure to select the product window does not diagnose a TCC fault.

The next attempts used the existing disposable AppKit window fixture, which paints changing content and closes after 180 seconds. No replacement viewer or decoder was used. The two successful runs confirmed `Window(14299)` / `Window(14300)`, responsive peers and video confirmation, with no video or surface errors. Connection IDs changed from `1573440878` to `3811600736`. The first GUI closed its sessions and exited, the normal service was restored, and the second run started a fresh GUI connection. This verifies process-level disconnect/reconnect; same-process reconnect remains untested.

The native surface reported zero CPU pixel bytes. GPU draw submissions/completions were 737/736 and 696/695, respectively. Imports are resource counts, not frame counts. The final held decoded frame had `exact_frame_submitted=false` in both snapshots; earlier positive draw/completion counters remain in the raw evidence. GPU completion and scene submission do not establish visible screen presentation. No first-visible-frame timestamp or screenshot was obtained, and this is not long-duration, input, audio or visual acceptance.

## Reverse-direction boundary

Studio's installed original GUI made an actual responsive Relay connection to HO5, ID `926781910`. It did not start video. The installed Linux source enumerator exposes MainDisplay/Desktop and advertises no window capture, while the old original-GPUI automation hook can select only a Window. The hook uses `connect_silent_files`, which deliberately starts no implicit desktop stream. A diagnostic state of `source=MainDisplay` is therefore not proof that this source was selected or captured.

The available external Studio helper has no Accessibility or Screen Recording grant, and this session has no callable desktop interaction tool. No new permission was requested. These helper permissions say nothing about the signed RemotePlay app's own permissions. Completing this direction requires the ordinary GUI to select HO5's Desktop source, or a supported original-GPUI automation path for that operation. This run does not show that HO5's capture itself failed.

## Restoration and preservation

HO5's original service finished active/running with PID **2232948**, NRestarts 0. Studio's original `com.remoteplay.host` launch agent finished with PID **52066**. Both runtime metadata files report `restored-original-gpui`; both installed artifact hashes are unchanged. Fixture PID 51140 exited normally, and no fixture process remained.

Only RemotePlay's own user service/launch agent was temporarily stopped for bounded tests, then restored from the unchanged existing entry. No unrelated service, installed app, launch configuration, OS permission, signing policy or system capture default was changed. The ordinary old application's startup/maintenance rewrote its existing profile files; changed inodes are recorded, while UID ownership and 0600 modes remain correct. Tools did not inspect/export credential contents or create replacement credentials. HO5's public node ID was obtained through the installed application's maintenance command after requiring its existing private files and normal owner; no extra GUI/network runtime was started.

MacBook's existing administrator-recovery wait was left untouched. All 105 pre-existing dirty paths retained their exact bytes and Git status. The candidate package and twelve-direction matrix are unchanged; their candidate acceptance remains unknown.

Complete source-bound observations, remote file hashes, operation IDs and restoration receipts are saved in [studio-ho5-installed-baseline.json](evidence/studio-ho5-installed-baseline.json).
