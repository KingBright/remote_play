# HEVC parameter-set recovery regression

The Windows/Linux FFmpeg reader previously split pictures only at the next VCL
NAL. Repeated VPS/SPS/PPS and prefix SEI preceding a keyframe were therefore
attached to the preceding delta picture. A receiver awaiting a keyframe skips
that delta picture, including the parameter sets needed to initialize its decoder.

End the previous access unit before VPS/SPS/PPS, AUD or prefix SEI following VCL.
Keep suffix SEI with its preceding picture. This changes access-unit boundaries,
not encoded bytes, authentication, packet schemas, capture defaults or color data.

Validate from the primary main checkout with the existing lifecycle entry:

```sh
python3 -B scripts/test_stream_settings_model.py --mode hevc-access-units
```

On 2026-10-16, an opaque synthetic fixture on cube produced a valid 960x540 HEVC
Main/yuv420p sample. The first received unit contained NAL types 32,33,34,39,20.
Later repeated parameter sets appeared on a delta unit (1,32,33,34,39), followed
by a keyframe unit containing only type 21. The live native receiver decoded the
fixture nonce and changing frame numbers 218 and 229; ffprobe and FFmpeg also
accepted the saved sample. These establish a working initial video path, not GUI
presentation or keyboard acceptance.

Offline native replay omitted the initial access unit from the same 413429-byte
synthetic sample. Original boundaries yielded zero decoded frames, four errors,
and `No frame data or session not ready`. Corrected boundaries yielded 21 decoded
frames, one initial error before repeated parameter sets arrived, and an empty
final decoder status. The payload bytes were identical. Both replays used the
same native execution environment; sandbox-only VideoToolbox session creation
failure is a separate environmental result.

The historical standalone receiver did not retain its decoder input, so its exact
initial-loss event is not independently established. This regression demonstrates
the recovery defect without claiming that every earlier zero-frame run has the
same cause. The fix still requires the authorized unified release and device
validation; it has not been deployed by this regression task.
