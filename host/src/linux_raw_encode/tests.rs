use super::*;
use crate::linux_frame::{BorrowedPlane, FrameStamp, Transform};
use std::time::{Duration, Instant};

pub(crate) fn frame(pixel: PixelFormat, color: SourceColor, sequence: u64) -> OwnedFrame {
    let (width, height) = (128u32, 64u32);
    let mut y = Vec::new();
    let mut u = Vec::new();
    let mut v = Vec::new();
    let colors = [
        (40u8, 90u8, 240u8),
        (90, 54, 34),
        (145, 240, 110),
        (210, 128, 128),
    ];
    for row in 0..height {
        for column in 0..width {
            y.push(colors[usize::from(row >= height / 2) * 2 + usize::from(column >= width / 2)].0);
        }
    }
    for row in 0..height / 2 {
        for column in 0..width / 2 {
            let color =
                colors[usize::from(row >= height / 4) * 2 + usize::from(column >= width / 4)];
            u.push(color.1);
            v.push(color.2);
        }
    }
    let uv: Vec<_> = u.iter().zip(&v).flat_map(|(&u, &v)| [u, v]).collect();
    let data: Vec<&[u8]> = if pixel == PixelFormat::Nv12 {
        vec![&y, &uv]
    } else {
        vec![&y, &u, &v]
    };
    let planes: Vec<_> = data
        .iter()
        .enumerate()
        .map(|(index, data)| BorrowedPlane {
            data,
            mapping_offset: 0,
            chunk_offset: 0,
            chunk_size: data.len() as u32,
            stride: if index == 0 || pixel == PixelFormat::Nv12 {
                width as i32
            } else {
                width as i32 / 2
            },
        })
        .collect();
    OwnedFrame::copy_from(
        FrameFormat {
            width,
            height,
            pixel_format: pixel,
            color,
            crop: None,
            transform: Transform::Identity,
        },
        &planes,
        FrameStamp {
            generation: 9,
            sequence: Some(sequence),
            pipewire_pts_ns: Some(900_000_000 + sequence as i64),
            arrival_ts_us: remote_core::timing::quanta_now_us(),
        },
    )
    .unwrap()
}
pub(crate) fn known_color() -> SourceColor {
    SourceColor {
        range: 2,
        matrix: 3,
        transfer: 5,
        primaries: 1,
    }
}
fn settings() -> RawSettings {
    RawSettings {
        width: 64,
        height: 64,
        fps: 30,
        bitrate_kbps: 2000,
    }
}
pub(crate) async fn wait_for_writer(source: &RawHevcSource) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while source.input.state.lock().unwrap().latest.is_some() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("native fixture raw writer stalled");
}
async fn fixture_stream(pixel: PixelFormat, color: SourceColor) -> Vec<RawChunk> {
    let source = RawHevcSource::start(&frame(pixel, color, 0), settings()).unwrap();
    for sequence in 0..6 {
        source.submit(frame(pixel, color, sequence)).unwrap();
        wait_for_writer(&source).await;
    }
    source.finish_input();
    let mut chunks = Vec::new();
    for sequence in 0..6 {
        let chunk = tokio::time::timeout(Duration::from_secs(5), source.pull())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.stamp.sequence, Some(sequence));
        assert_eq!(
            chunk.stamp.pipewire_pts_ns,
            Some(900_000_000 + sequence as i64)
        );
        assert!(chunk.encode_done_ts_us >= chunk.stamp.arrival_ts_us);
        if sequence == 0 {
            assert!(chunk.is_keyframe);
        }
        chunks.push(chunk);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(2), source.pull())
            .await
            .unwrap()
            .is_err()
    );
    chunks
}

// Only synthetic stdin input. Child processes are bounded, and stdout is drained
// while they run. No screen device, UI, PipeWire remote or system portal opened.
fn inspect(program: &str, arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(1_048_576).read_to_end(&mut bytes).unwrap();
        bytes
    });
    child.stdin.take().unwrap().write_all(input).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let success = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.success();
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = reader.join().unwrap();
    assert!(success, "bounded synthetic codec inspection failed");
    bytes
}
fn encoded(chunks: &[RawChunk]) -> Vec<u8> {
    chunks
        .iter()
        .flat_map(|chunk| chunk.nalu.iter().copied())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn actual_ffmpeg_nv12_retains_quadrants_sps_and_every_submitted_timing() {
    let chunks = fixture_stream(PixelFormat::Nv12, known_color()).await;
    let bytes = encoded(&chunks);
    let metadata = String::from_utf8(inspect(
        "ffprobe",
        &[
            "-v",
            "error",
            "-f",
            "hevc",
            "-show_entries",
            "stream=width,height,color_range,color_space,color_transfer,color_primaries",
            "-of",
            "compact=p=0",
            "pipe:0",
        ],
        &bytes,
    ))
    .unwrap();
    assert!(metadata.contains("width=64|height=32"), "{metadata}");
    for value in [
        "color_range=tv",
        "color_space=bt709",
        "color_transfer=bt709",
        "color_primaries=bt709",
    ] {
        assert!(metadata.contains(value), "{metadata}");
    }
    let decoded = inspect(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "hevc",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        &bytes,
    );
    assert_eq!(decoded.len(), 64 * 32 * 3 / 2);
    for (value, (x, y)) in [
        (40u8, (16, 8)),
        (90, (48, 8)),
        (145, (16, 24)),
        (210, (48, 24)),
    ] {
        assert!(
            decoded[y * 64 + x].abs_diff(value) <= 6,
            "quadrant Y: expected {value}, got {}",
            decoded[y * 64 + x]
        );
    }
    for (offset, expected) in [
        (64 * 32, [90u8, 54, 240, 128]),
        (64 * 32 + 32 * 16, [240u8, 34, 110, 128]),
    ] {
        for (value, (x, y)) in expected
            .into_iter()
            .zip([(8, 4), (24, 4), (8, 12), (24, 12)])
        {
            assert!(
                decoded[offset + y * 32 + x].abs_diff(value) <= 6,
                "quadrant chroma: expected {value}, got {}",
                decoded[offset + y * 32 + x]
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn actual_ffmpeg_i420_unknown_matrix_stays_unknown_in_sps() {
    let chunks = fixture_stream(
        PixelFormat::I420,
        SourceColor {
            range: 2,
            ..SourceColor::default()
        },
    )
    .await;
    let metadata = String::from_utf8(inspect(
        "ffprobe",
        &[
            "-v",
            "error",
            "-f",
            "hevc",
            "-show_entries",
            "stream=color_space,color_transfer,color_primaries",
            "-of",
            "compact=p=0",
            "pipe:0",
        ],
        &encoded(&chunks),
    ))
    .unwrap();
    for value in [
        "color_space=unknown",
        "color_transfer=unknown",
        "color_primaries=unknown",
    ] {
        assert!(metadata.contains(value), "{metadata}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn actual_ffmpeg_full_range_yuv_retains_samples_and_sps() {
    let mut color = known_color();
    color.range = 1;
    let chunks = fixture_stream(PixelFormat::I420, color).await;
    let bytes = encoded(&chunks);
    let metadata = String::from_utf8(inspect(
        "ffprobe",
        &[
            "-v",
            "error",
            "-f",
            "hevc",
            "-show_entries",
            "stream=color_range,color_space",
            "-of",
            "compact=p=0",
            "pipe:0",
        ],
        &bytes,
    ))
    .unwrap();
    assert!(metadata.contains("color_range=pc"), "{metadata}");
    assert!(metadata.contains("color_space=bt709"), "{metadata}");
    let decoded = inspect(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "hevc",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-pix_fmt",
            "yuvj420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ],
        &bytes,
    );
    assert_eq!(decoded.len(), 64 * 32 * 3 / 2);
    for (value, (x, y)) in [
        (40u8, (16, 8)),
        (90, (48, 8)),
        (145, (16, 24)),
        (210, (48, 24)),
    ] {
        assert!(
            decoded[y * 64 + x].abs_diff(value) <= 6,
            "full-range Y changed: {value} -> {}",
            decoded[y * 64 + x]
        );
    }
}

#[test]
fn reference_au_backpressure_preserves_order_and_multislice_frames() {
    let mut bytes = Vec::new();
    for sequence in 0..12u8 {
        bytes.extend([0, 0, 1, 35 << 1, 1, 0x50]);
        bytes.extend([0, 0, 1, 1 << 1, 1, 0x80, sequence]);
        bytes.extend([0, 0, 1, 1 << 1, 1, 0x00, sequence]);
    }
    let (stamps_tx, stamps_rx) = sync_mpsc::sync_channel(16);
    for sequence in 0..12u64 {
        stamps_tx
            .send(FrameStamp {
                generation: 1,
                sequence: Some(sequence),
                pipewire_pts_ns: None,
                arrival_ts_us: sequence + 1,
            })
            .unwrap();
    }
    drop(stamps_tx);
    let (tx, mut rx) = mpsc::channel(4);
    let reader =
        std::thread::spawn(move || drain_reader(std::io::Cursor::new(bytes), stamps_rx, tx));
    // Let the real reader saturate its compressed queue before consuming.
    let until = Instant::now() + Duration::from_secs(2);
    while rx.len() < 4 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(rx.len(), 4);
    for sequence in 0..12u64 {
        let chunk = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(chunk.stamp.sequence, Some(sequence));
        assert_eq!(starts(&chunk.nalu).len(), 3);
    }
    assert!(rx.blocking_recv().is_none());
    reader.join().unwrap();
}

#[test]
fn unsubmitted_slot_keeps_latest_and_rejects_malformed_or_unknown_rgb() {
    let input = Input::default();
    input
        .submit(frame(PixelFormat::Nv12, known_color(), 1))
        .unwrap();
    input
        .submit(frame(PixelFormat::Nv12, known_color(), 2))
        .unwrap();
    assert_eq!(input.receive().unwrap().stamp.sequence, Some(2));
    let mut invalid = frame(PixelFormat::I420, known_color(), 1);
    invalid.planes[0].pop();
    assert!(RawHevcSource::start(&invalid, settings()).is_err());
    let mut rgb = frame(PixelFormat::I420, SourceColor::default(), 1);
    rgb.source_format.pixel_format = PixelFormat::Bgra;
    assert!(raw_command(&rgb, settings()).is_err());
    let mut hdr = known_color();
    hdr.transfer = 14;
    assert!(color_values(hdr).is_err());
    assert!(color_values(SourceColor::default()).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_unblocks_full_compressed_queue_and_reaps_ffmpeg() {
    let source =
        RawHevcSource::start(&frame(PixelFormat::Nv12, known_color(), 0), settings()).unwrap();
    let pid = source.child.id();
    for sequence in 0..10 {
        source
            .submit(frame(PixelFormat::Nv12, known_color(), sequence))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while source.output.lock().await.len() < 4 && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(source.output.lock().await.len(), 4);
    let started = Instant::now();
    drop(source);
    assert!(started.elapsed() < Duration::from_secs(2));
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .unwrap();
    assert!(
        output.stdout.is_empty(),
        "fixture encoder PID was not reaped"
    );
}
