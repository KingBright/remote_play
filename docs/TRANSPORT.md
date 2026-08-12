# Unified High-Performance Data Plane

## Goal

All application traffic should eventually share one transport data plane: video, audio, input/control, clipboard sync, file transfer, and future arbitrary payloads. Unification must not mean a slow abstraction layer. The data plane must expose different lanes with different delivery semantics while keeping packet handling as close to the current realtime path as possible.

## Core Principle

Realtime and non-realtime data have different correctness rules.

- Realtime data values freshness over completeness. Video, audio, input, heartbeat, and session control must avoid head-of-line blocking, carry deadlines, prefer newest useful data, and drop stale packets when waiting would hurt interaction.
- Non-realtime data values completeness over immediacy. Clipboard payloads, files, and arbitrary large objects may tolerate higher latency, but need reliable delivery, checksums, retries, cancellation, progress, and backpressure.

## Lanes

| Lane | Examples | Reliability | Ordering | Scheduling |
| --- | --- | --- | --- | --- |
| RealtimeVideo | H.265 frame/NAL payloads | Best effort | Sequence-aware, skip stale | Highest throughput, deadline-first |
| RealtimeAudio | Opus packets | Best effort | Sequence-aware, jitter-buffered | Deadline-first, low jitter |
| InteractiveControl | input, heartbeat, start/stop, telemetry | Best effort or small reliable ack where needed | Mostly latest-state wins | Higher priority than bulk data |
| ReliableObject | clipboard, file chunks | Reliable | Ordered per stream | Uses spare bandwidth, backpressure |
| Background | future sync/metadata | Reliable or best effort by kind | Per stream | Lowest priority |

## Design Constraints

- The envelope must be cheap: compact binary header, minimal copies, no heap-heavy dynamic metadata on realtime packets.
- Audio/video migration requires benchmark proof. The unified path is not accepted if it measurably regresses packet size, allocation count, throughput, or latency without a compensating gain.
- Realtime lanes must never wait behind file transfer, clipboard sync, or arbitrary bulk data.
- Non-realtime lanes must be cancelable and resumable enough to avoid poisoning the realtime path.
- The scheduler owns fairness. Call sites should declare lane, stream id, priority, and deadline; the transport decides send order.
- Fragmentation must be lane-aware. Realtime fragmentation should be aggressively bounded; large reliable objects should use explicit chunk streams with acknowledgements.
- Platform APIs must remain outside the transport. macOS, Windows, and Linux capture/clipboard/file/input adapters all feed the same protocol and data-plane contracts described in `docs/PLATFORM.md`.
- Cross-platform support must not add mandatory copies to the realtime path. The transport accepts platform-neutral envelopes, but media backends still need native zero-copy or low-copy frame handles where the OS supports them.

## Migration Shape

1. Define the shared envelope and lane model in `protocol`.
2. Add tests and microbenchmarks around envelope encode/decode overhead.
3. Implement realtime lane send/receive adapters that can carry the existing video/audio payloads.
4. Keep the old RTP-like path available until benchmarks and local stream tests prove parity.
5. Add reliable object streams for clipboard/file transfer.
6. Add QoS scheduling across realtime and non-realtime lanes.

## Benchmarking

Run the repeatable protocol-level benchmark with:

```bash
cargo run -p protocol --example transport_bench --release -- 50000
```

Initial result on 2026-05-15: the full realtime `DataEnvelope` bincode form is 36 bytes larger than the current RTP-like packet for tested payload sizes. This is acceptable for experimentation, but not enough to approve audio/video migration by itself.

To keep the unified data plane from becoming a slow abstraction, realtime packets now also support a compact fixed header wire format. `UdpSender::send_data` selects this format automatically for realtime envelopes and sends it with multiplexing header `0x05`; non-realtime and reliable-object envelopes keep using the full `0x04` bincode form.

Protocol-level benchmark, 20,000 iterations, 2026-05-15:

| Payload | RTP size | Full envelope | Compact realtime | Compact overhead | Compact encode/decode |
| --- | ---: | ---: | ---: | ---: | ---: |
| 64 B | 84 B | 120 B | 90 B | +6 B / 7.14% | ~29.6 ns / ~30.7 ns |
| 512 B | 532 B | 568 B | 538 B | +6 B / 1.13% | ~39.2 ns / ~41.0 ns |
| 1,400 B | 1,420 B | 1,456 B | 1,426 B | +6 B / 0.42% | ~43.2 ns / ~44.4 ns |
| 8,000 B | 8,020 B | 8,056 B | 8,026 B | +6 B / 0.07% | ~126.0 ns / ~159.9 ns |
| 64,000 B | 64,020 B | 64,056 B | 64,026 B | +6 B / 0.01% | ~1.14 us / ~1.22 us |

Loopback UDP benchmark, 5,000 iterations, 2026-05-15:

| Payload | Current RTP path | Compact realtime data path |
| --- | ---: | ---: |
| 64 B | ~19.4 us/packet | ~19.7 us/packet |
| 512 B | ~16.9 us/packet | ~18.8 us/packet |
| 1,400 B | ~24.9 us/packet | ~24.7 us/packet |
| 8,000 B | ~72.2 us/packet | ~54.8 us/packet |

Media adapter benchmark, 20,000 iterations, 2026-05-17:

This benchmark compares the current RTP-like bincode roundtrip against the guarded migration path: RTP-like packet -> realtime `DataEnvelope` -> compact realtime wire -> realtime `DataEnvelope` -> RTP-like packet.

| Payload | RTP size | Compact data size | Legacy RTP roundtrip | Data-plane adapter roundtrip |
| --- | ---: | ---: | ---: | ---: |
| 64 B | 84 B | 90 B | ~132.7 ns | ~90.7 ns |
| 512 B | 532 B | 538 B | ~1.10 us | ~165.0 ns |
| 1,400 B | 1,420 B | 1,426 B | ~1.66 us | ~167.9 ns |
| 8,000 B | 8,020 B | 8,026 B | ~8.44 us | ~415.1 ns |
| 64,000 B | 64,020 B | 64,026 B | ~66.68 us | ~6.53 us |

Conclusion: the compact realtime format fixes the obvious full-envelope overhead and is close enough to justify guarded runtime testing. The host can now opt into media-over-data-plane with `REMOTE_PLAY_DATA_PLANE_MEDIA=1`; the client accepts both legacy RTP-like media packets and realtime data-plane media packets through the same jitter/decode path. This still does not approve deleting the old RTP-like path. Before migration is accepted, measure allocation count, end-to-end latency, jitter, packet loss behavior, mixed traffic with reliable object transfers active, and at least one real host/client smoke session.

## Media Adapter Notes

- Video packets preserve the existing sequence number, session SSRC, payload type, capture timestamp, and payload bytes. Because the current RTP timestamp is a wrapped `u32` millisecond capture time, the adapter unwraps it to the `u64` millisecond timestamp nearest the sender clock before deriving a short realtime deadline. Converting back to the RTP-like packet preserves the lower 32 bits.
- Audio packets preserve the existing sequence number, audio SSRC, Opus payload type, RTP sample-clock timestamp, and payload bytes. Host audio capture chunks arbitrary device callback buffers into fixed 20 ms Opus frames before encoding, so the transport sees packetized low-latency audio rather than device-sized buffers. The adapter intentionally leaves `deadline_ms` empty for audio until the audio path carries a true wall-clock send/capture timestamp; treating a sample-clock timestamp as milliseconds would make stale-drop decisions wrong.
- Audio stream metadata is separated from the Opus hot path. `ContentKind::AudioStreamConfig` travels on the interactive-control lane and names the stream id, audio source, direction, codec, sample rate, channel count, and frame duration. Individual Opus packets remain `ContentKind::AudioOpus` on the compact realtime path, keyed by stream id, so adding remote system audio, remote microphone audio, and viewer microphone talkback does not add per-packet metadata overhead.
- Current session-derived audio stream ids are fixed in `protocol`: remote microphone is `session_id + 1`, remote system audio is `session_id + 2`, and viewer microphone talkback is `session_id + 100` with wrapping arithmetic. This keeps the hot path keyed by a single stream id while making both host-to-client and client-to-host audio coexist in one program model.
- The client audio player now maintains Opus decoders per audio stream id. When metadata is available it decodes with the announced mono/stereo layout and maps the result into the output device channel count. The media receiver admits the default remote microphone stream and any configured host-to-client system-audio stream for the active session; legacy RTP-like audio still falls back to 48 kHz stereo until the old path is retired or given a control-plane config.
- Viewer microphone talkback is guarded by `REMOTE_PLAY_TALKBACK=1`. The client captures the viewer microphone, periodically announces one `ViewerMicrophoneTalkback` stream config, and then sends Opus packets over the realtime audio lane. Periodic config refresh avoids a startup race where the host has not yet installed the active-session talkback route. The host only accepts the active session's client-to-host talkback config and plays matching packets, so enabling talkback does not change default streaming behavior.
- Audio controls stay out of the realtime packet hot path. Client-side remote system/microphone mute and volume are applied locally in the audio player. Talkback local-mic mute and push-to-talk gate encoding/sending before Opus packets are produced. `ControlMessage::AudioControl` is reserved for active-session playback controls that must affect the controlled machine, currently viewer-talkback playback mute and volume.
- Realtime media packets and reliable object packets have separate wire paths: compact `0x05` for realtime media, full `0x04` for reliable/non-realtime objects. This is covered by UDP roundtrip tests, but full QoS scheduling under mixed realtime plus bulk traffic still belongs to Step 3.7.

## Scheduling

`remote_core::data_plane::LaneScheduler` provides the first scheduling primitive for mixed traffic:

- Realtime envelopes are ordered by priority, deadline, then sequence number.
- Reliable and background envelopes are kept in FIFO order.
- Realtime envelopes always pop before reliable/bulk envelopes.
- Stale realtime video/audio envelopes are dropped instead of blocking reliable work.
- Queue capacity is explicit. When the realtime queue is full, the lowest urgency realtime packet is dropped. When the reliable queue is full, new reliable/bulk payloads are rejected so callers can apply backpressure or retry later.
- Scheduler stats expose queued realtime count, queued reliable count, stale realtime drops, realtime capacity drops, and reliable capacity rejections.

This scheduler is currently a tested in-memory primitive. It is not yet wired into the live UDP sender loop; that integration should happen with mixed-traffic benchmarks so file/clipboard transfers can consume spare bandwidth without delaying video/audio/input.

Repeatable scheduler mixed-traffic benchmark:

```bash
cargo run -p remote_core --example scheduler_mixed_traffic_bench --release -- 10000 4 2 2
```

The arguments are `ticks`, `bulk_per_tick`, `send_budget_per_tick`, and `realtime_every_ticks`. In the default heavy-bulk scenario above, 5,000 realtime packets were generated and all were sent with 0 tick average/max wait; 24,874 bulk payloads were rejected by reliable-queue backpressure. In a tighter `10000 8 1 1` scenario, all 10,000 realtime packets were sent with 0 tick average/max wait while bulk was almost entirely backpressured. This confirms the scheduler rule we want before wiring it into real transport: bulk traffic uses spare capacity only.

`remote_core::scheduled_sender::ScheduledDataSender` is the first live sender integration. It accepts `DataEnvelope` values through a bounded channel, batches them on a configurable tick, feeds them into `LaneScheduler`, and sends the chosen envelopes through the existing `UdpSender::send_data` path. A loopback test confirms that when bulk is queued before realtime and the send budget is one packet per tick, the realtime packet is transmitted first.

The host's `REMOTE_PLAY_DATA_PLANE_MEDIA=1` media path now uses `ScheduledDataSender`. The default RTP-like path is unchanged. Data-plane media submission uses non-blocking `try_send`; if the scheduled sender entrance is full, that realtime media packet is dropped instead of stalling capture or encode. When clipboard sync or file transfer is enabled for the same active stream, the host reuses that same scheduled sender so media, clipboard, and file bytes compete inside one lane-aware scheduler instead of separate UDP send loops.

Scheduled media loopback coverage now exercises the actual smoke path without macOS capture or UI: RTP-like video/audio packets -> media adapter -> `ScheduledDataSender` -> UDP loopback -> media adapter back to RTP-like packets. The test also queues bulk first and verifies media still arrives before bulk under a one-packet send budget.

A pressure regression test now queues more reliable-object packets than the scheduler will hold, then submits realtime video and audio. The first two packets received on UDP are still the media packets, while reliable overflow is recorded as backpressure. A higher-level file-transfer regression also queues real file-transfer packets first, submits video/audio through the same sender, verifies UDP observes video then audio before file packets, and still checks that the file lands intact. These are the current automated guards for the rule that bulk data may be rejected or delayed, but realtime media must stay ahead.

`ScheduledDataSender::stats()` exposes a lightweight snapshot for runtime smoke tests: entrance queue depth, entrance enqueue/full/closed counts, scheduler stale/capacity/reliable rejection counts, sent realtime/reliable counts, and send error count. These counters are intentionally cheap atomics so the host can sample them during an experimental run without adding logging to the hot path.

When `REMOTE_PLAY_DATA_PLANE_MEDIA=1` is enabled, the host samples those counters once per second during an active stream. The output is intentionally limited to the experimental path and reports entrance queue pressure, realtime stale/capacity drops, reliable-object rejections, sent realtime/reliable packet counts, and send errors. This gives manual smoke tests a quick signal for whether the unified sender is staying clear or dropping under pressure.

`scripts/smoke_dataplane_media_file.sh` is the first repeatable runtime smoke for this path. It starts the unified app in headless passive-host mode with data-plane media and file transfer enabled, runs a headless protocol client, verifies data-plane video packets and a checksum-verified host-to-client file transfer, then stops the stream. Set `REMOTE_PLAY_EXPECT_AUDIO=1` to require data-plane audio packets as well. See `docs/RUNTIME_SMOKE.md` for the exact command and latest local result.

## Clipboard Payloads

Clipboard sync uses a `ClipboardBundle` payload rather than a text-only message. A bundle can contain text, image, and file items. Images carry a MIME type, optional dimensions, and bytes. File items carry a name, optional MIME type, and bytes, which gives us a path toward cross-machine file clipboard paste instead of only copying local file paths.

`remote_core::clipboard_plane` encodes each bundle as reliable `ContentKind::ClipboardBundle` object chunks with CRC32 integrity and reassembles them with the existing reliable object assembler. A UDP loopback test sends a mixed text/image/file clipboard bundle through `UdpSender::send_data` and receives it as `MultiplexedPacket::Data`, proving the feature is on the shared data plane.

`ClipboardSyncPolicy` is the first privacy and safety gate. Text and image clipboard items are allowed by default, but file clipboard items require explicit opt-in before their bytes are sent. The policy also caps text, image, file, and total encoded bundle sizes so app-level integration can reject unexpectedly large clipboard payloads before they enter the transport queue.

The first OS-backed adapter is macOS-native and uses NSPasteboard for text plus PNG/TIFF images. File clipboard remains a separate capability because cross-machine file paste needs safe file URL handling and, for larger files, the file-transfer stream rather than full in-memory bundle copies.

`remote_core::clipboard_sync::ClipboardSyncEndpoint` is the platform-neutral service state machine that sits above these primitives. It polls a `ClipboardProvider`, turns changed clipboard bundles into reliable data-plane envelopes, receives peer clipboard envelopes, reassembles bundles, writes them back through the provider, and suppresses echo loops by comparing normalized content checksums that ignore the per-transfer bundle id.

Runtime integration should wire this endpoint into the existing scheduler path rather than sending clipboard traffic directly. Outgoing envelopes should enter `ScheduledDataSender` or the equivalent lane-aware sender as `ReliableObject` traffic, so text/image/file clipboard payloads can use spare bandwidth while realtime media and interactive control stay ahead. Incoming `MultiplexedPacket::Data` values with `ContentKind::ClipboardBundle` should be handed to the endpoint; other content kinds remain owned by their respective services.

The first guarded runtime integration is behind `REMOTE_PLAY_CLIPBOARD_SYNC=1`. In that mode, host and client start clipboard sync tasks for an active stream, route inbound `ContentKind::ClipboardBundle` packets to the endpoint, and submit outgoing clipboard chunks through `ScheduledDataSender`. When media-over-data-plane is also enabled on the host, media and clipboard share the same scheduled sender so reliable clipboard traffic remains behind realtime audio/video.

This is still an experimental transport/service foundation. The remaining app work is adding user-facing opt-in/privacy controls, carrying the integration into the future unified runtime, normalizing image formats where needed, and adding a streaming file-transfer path for very large file clipboard entries so large files do not require a full in-memory bundle.

## File Transfer

File transfer uses the same reliable-object data plane but avoids wrapping every file chunk in an extra serialized structure. The wire shape is:

- `ContentKind::FileManifest`: a small reliable object that carries `FileTransferManifest` with transfer id, file object id, optional file-group metadata, safe file name, optional MIME type, total size, chunk payload length, and full-file CRC32.
- `ContentKind::FileChunk`: raw file bytes as reliable object chunks. The outer `ChunkInfo` carries file object id, chunk index, offset, total chunk count, total size, and the full-file checksum.
- `ContentKind::FileControl`: a small transfer-control object for cancellation. `CancelTransfer` targets one transfer/file object; `CancelGroup` targets a whole grouped transfer. It is sent through the full data envelope with interactive priority so it can bypass bulk payloads in the scheduler while still carrying explicit checksum metadata.

`remote_core::file_transfer::FileTransferReader` prepares a transfer without keeping the whole file in memory. It streams the file once to compute CRC32 and size, then streams it again to produce manifest plus fixed-size raw file chunks. This is intentionally non-realtime work: it can take spare bandwidth and backpressure from `ScheduledDataSender`, but it must never block media capture, encode, decode, or input.

`IncomingFileTransfer` materializes file chunks directly to disk using validated offsets and safe file names, supports out-of-order chunk arrival through random writes, and verifies the final file checksum before marking the transfer complete. The receive policy rejects unsafe names and enforces size limits; overwrite is explicit and disabled by default.

`remote_core::file_transfer_runtime::run_file_transfer_runtime` is the service layer above those primitives. It accepts `SendFile`, `SendFileGroup`, cancellation, and receive-config commands, emits outgoing/incoming progress events, sends file envelopes through `ScheduledDataSender`, consumes inbound `FileManifest`/`FileChunk` envelopes, and materializes completed files to the configured receive directory. Receive directory and overwrite changes apply immediately when the inbound side is idle; if a file or grouped transfer is already active, the new config is held until the next idle boundary so one group is not split across destinations. A runtime loopback test covers source command -> scheduled sender -> UDP data plane -> target runtime -> disk.

`FileTransferGroup` preserves multi-file clipboard semantics across the data plane. Each file manifest can carry a group id, file index, file count, relative path, aggregate byte count, and group checksum. `SendFileGroup` expands directory references into regular file entries before streaming, using relative paths such as `Project/nested/file.txt` so the receiver can rebuild the directory shape. The receiver still verifies each file independently before publishing it as complete, then emits `IncomingGroupCompleted` only after every file in the group has materialized and verified. This gives the app a single atomic completion point for "copy these files together" workflows.

Cancellation is now part of the file-transfer runtime rather than a UI-only concern. `CancelTransfer` stops the local reader and sends remote cleanup control for the target file object. `CancelGroup` stops grouped sends and tells the receiver to remove both partial files and already materialized files from the unfinished group. Cancelled groups do not emit `IncomingGroupCompleted`, so file clipboard does not publish cancelled paths. The client control panel includes a first transfer-center view that subscribes to these events and can issue cancel commands. It also exposes receive directory selection, an overwrite toggle, and file/folder picker buttons for manual sends; selected paths enter the same `SendFile`/`SendFileGroup` runtime commands used by the rest of the app.

The first guarded host/client integration is behind `REMOTE_PLAY_FILE_TRANSFER=1`. Both sides route inbound `ContentKind::FileManifest` and `ContentKind::FileChunk` data-plane packets into the runtime. For manual smoke tests, `REMOTE_PLAY_SEND_FILE=/path/to/file` makes the client send a file after connecting, and `REMOTE_PLAY_HOST_SEND_FILE=/path/to/file` makes the host send a file after accepting `StartStream`. `REMOTE_PLAY_FILE_RECEIVE_DIR=/path/to/dir` overrides the initial receive directory; otherwise each side uses an app-specific directory under the system temp directory. `REMOTE_PLAY_FILE_ALLOW_OVERWRITE=1` enables overwrite in the client's initial runtime settings, while the client GUI can change both the destination and overwrite policy at runtime.

File clipboard is layered on top of file transfer rather than encoding file bytes inside `ClipboardBundle`. `ClipboardFileReferenceProvider` exposes local OS clipboard file references as paths. `remote_core::clipboard_file_runtime::run_clipboard_file_sync` polls those references and issues `SendFileGroup` commands, then writes received file paths back to the OS clipboard only after `IncomingGroupCompleted`. For directory references, the group completion paths are the rebuilt top-level directories rather than every internal file, matching normal paste behavior. Single-file non-group transfers still publish after `IncomingCompleted`. The first guarded host/client integration is behind `REMOTE_PLAY_FILE_CLIPBOARD=1`, which implies the file-transfer runtime must be active.

This foundation is ready for broader app-level file transfer and file clipboard integration. The next layer should improve transfer-center polish, add stronger user-facing privacy controls, and eventually add resumable retry/ack flow.
