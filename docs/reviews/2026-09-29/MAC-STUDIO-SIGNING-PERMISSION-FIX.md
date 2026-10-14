# Mac Studio: permission enabled but ScreenCaptureKit rejected access

## Confirmed cause

The user reported Screen Recording was already enabled. The system log confirmed an existing authorization record for com.remoteplay.unified requiring the original signing certificate, whereas the installed alpha.1 app was ad-hoc signed. This was a deployment identity mismatch, not evidence that the user forgot to enable permission.

TCC log from the running host after a restart:

```
Failed to match existing code requirement for subject com.remoteplay.unified and service kTCCServiceScreenCapture
identifier "com.remoteplay.unified" and certificate leaf = H"8eaa97365d97b8a987cdf1232430eddf9aba8ecf"
cdhash H"349d99fe6681cab8fae020e81e0288d87342ab69"
```

The app identity was attributed to the correct /Users/jinliang/Applications/RemotePlay.app/Contents/MacOS/remote_play, not the diagnostic client or Remote Hosts. Codesign verification with the exact saved requirement failed for the installed old app and succeeded for the existing NAS alpha.4 release signed by RemotePlay Local.

## Actions and evidence

1. Restarted only com.remoteplay.host with the old executable unchanged. PID 69814, originally started on September 24, became 95334. A bounded authenticated ListSources request to the running host still returned the original TCC error. This ruled out a simple restart as the complete remedy.
2. Downloaded the previously published alpha.4 macOS archive. SHA-256: a96a4f442286909c6031fb0fc4f77aa628e5960b4085a4953594c5b4fefd35f9.
3. Verified the new app against the original certificate requirement before installation. Preserved the old app under /Users/jinliang/workspace/.rp-studio-permission-20260929/RemotePlay.before-signing-fix.app.
4. Replaced the old app at the existing installation path and restarted the existing launch agent without changing its configuration. New PID 99454, alpha.4, service running.
5. Repeated the same authenticated source-list request to the actual running host. It succeeded with 57 shareable sources, including Display(10), 1920x1080. No source titles were logged by the diagnostic client.
6. The app's network configuration and launch-agent configuration were hash-checked and unchanged. No TCC database read/write, permission reset, automatic approval, or network change was performed. UU and unrelated services were not restarted.

## Remaining evidence boundary

The ListSources probe did not start a video subscription. The subsequent attempted HO5 GUI video check was blocked by the platform before execution. Therefore actual HO5 decoded/displayed frames are not established by this fix receipt; the user should close the stale Mac Studio session and reconnect normally. Do not describe this as completed video/input/long-duration acceptance.

## Release follow-up

A public macOS release must use the stable authorized signing identity. Ad-hoc signing must not be an automatic fallback for an installed production app. Existing distribution packaging should fail closed for releases when the expected identity is unavailable, rather than creating another permission-incompatible build.

Reference: Apple TN3127, Inside Code Signing: Requirements.
