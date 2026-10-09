//! Read-only X11 geometry for the existing compatibility capture path.
//! Preserve the existing root-window source and desktop input coordinates.
//! X11 root geometry does not establish native Wayland desktop coverage.
use std::process::Command;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct X11CaptureRegion {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Resolve only metadata. Do not read pixels, change display modes or guess a
/// source from a requested encoded size. A failed probe must precede FFmpeg spawn.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn capture_region(display: &str) -> Result<X11CaptureRegion, String> {
    let result = (|| -> Result<_, Box<dyn std::error::Error + Send + Sync>> {
        let (connection, screen_index) = x11rb::connect(Some(display))?;
        let root = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or("missing X11 screen")?
            .root;
        let geometry = connection.get_geometry(root)?.reply()?;
        Ok(root_region(
            u32::from(geometry.width),
            u32::from(geometry.height),
        )?)
    })();
    result.map_err(|error| {
        format!("X11 compatibility capture unavailable: {error}. No capture process was started.")
    })
}

fn root_region(width: u32, height: u32) -> Result<X11CaptureRegion, String> {
    validate_region(
        X11CaptureRegion {
            x: 0,
            y: 0,
            width,
            height,
        },
        (width, height),
    )
}

fn validate_region(
    region: X11CaptureRegion,
    root_size: (u32, u32),
) -> Result<X11CaptureRegion, String> {
    if region.x < 0
        || region.y < 0
        || region.width < 2
        || region.height < 2
        || u64::from(region.width) + region.x as u64 > u64::from(root_size.0)
        || u64::from(region.height) + region.y as u64 > u64::from(root_size.1)
    {
        return Err(
            "X11 capture rectangle is outside the current root; retry after the display change"
                .into(),
        );
    }
    if region.width > 8192
        || region.height > 8192
        || u64::from(region.width) * u64::from(region.height) > 33_554_432
    {
        return Err("X11 source exceeds the host capture budget".into());
    }
    Ok(region)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct X11CapturePlan {
    pub source: X11CaptureRegion,
    pub output_width: u32,
    pub output_height: u32,
}

impl X11CapturePlan {
    pub fn new(source: X11CaptureRegion, max_width: u32, max_height: u32) -> Result<Self, String> {
        if source.width < 2 || source.height < 2 || max_width < 2 || max_height < 2 {
            return Err("capture and encoded dimensions must each be at least two pixels".into());
        }
        let bound_width = source.width.min(max_width);
        let bound_height = source.height.min(max_height);
        // Fit the entire source into the requested bounds without upscaling.
        // Round only the final dimensions for the existing 4:2:0 HEVC encoder.
        let (width, height) = if u64::from(source.height) * u64::from(bound_width)
            <= u64::from(source.width) * u64::from(bound_height)
        {
            (
                bound_width,
                (u64::from(source.height) * u64::from(bound_width) / u64::from(source.width))
                    as u32,
            )
        } else {
            (
                (u64::from(source.width) * u64::from(bound_height) / u64::from(source.height))
                    as u32,
                bound_height,
            )
        };
        let (output_width, output_height) = (width & !1, height & !1);
        if output_width < 2 || output_height < 2 {
            return Err("source aspect ratio cannot fit the requested HEVC dimensions".into());
        }
        Ok(Self {
            source,
            output_width,
            output_height,
        })
    }

    pub fn append_args(&self, command: &mut Command, display: &str, fps: u32) {
        command.args([
            "-f",
            "x11grab",
            "-video_size",
            &format!("{}x{}", self.source.width, self.source.height),
            "-framerate",
            &fps.to_string(),
            "-i",
            &format!("{display}+{},{}", self.source.x, self.source.y),
            "-vf",
            &format!("scale=w={}:h={}", self.output_width, self.output_height),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: i32, y: i32, width: u32, height: u32) -> X11CaptureRegion {
        X11CaptureRegion {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn full_4k_source_is_scaled_instead_of_cropped_to_1080p() {
        let plan = X11CapturePlan::new(root_region(3840, 2160).unwrap(), 1920, 1080).unwrap();
        let mut command = Command::new("fixture-ffmpeg");
        plan.append_args(&mut command, ":0.0", 60);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-video_size", "3840x2160"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-i", ":0.0+0,0"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-vf", "scale=w=1920:h=1080"])
        );
        assert_eq!((plan.output_width, plan.output_height), (1920, 1080));
    }

    #[test]
    fn source_size_and_offset_are_independent_of_encoded_size() {
        let source = validate_region(region(1920, 120, 2560, 1600), (4480, 1720)).unwrap();
        let plan = X11CapturePlan::new(source, 1280, 720).unwrap();
        let mut command = Command::new("fixture-ffmpeg");
        plan.append_args(&mut command, ":2.1", 30);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-video_size", "2560x1600"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-i", ":2.1+1920,120"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-vf", "scale=w=1152:h=720"])
        );
        assert_eq!((plan.output_width, plan.output_height), (1152, 720));
    }

    #[test]
    fn portrait_small_and_odd_sources_fit_without_upscaling_or_cropping() {
        for (source, requested, expected) in [
            ((1080, 1920), (1920, 1080), (606, 1080)),
            ((1280, 720), (1920, 1080), (1280, 720)),
            ((1919, 1079), (1920, 1080), (1918, 1078)),
            ((3840, 2160), (1280, 720), (1280, 720)),
        ] {
            let plan =
                X11CapturePlan::new(region(0, 0, source.0, source.1), requested.0, requested.1)
                    .unwrap();
            assert_eq!((plan.output_width, plan.output_height), expected);
            assert_eq!((plan.source.width, plan.source.height), source);
        }
    }

    #[test]
    fn root_source_and_desktop_coordinate_origin_are_preserved() {
        assert_eq!(root_region(3840, 2160).unwrap(), region(0, 0, 3840, 2160));
        assert_eq!(root_region(4480, 1720).unwrap(), region(0, 0, 4480, 1720));
    }

    #[test]
    fn missing_or_over_budget_root_never_guesses_requested_dimensions() {
        assert!(root_region(0, 0).is_err());
        assert!(root_region(8192, 8192).is_err());
        assert!(root_region(9000, 1080).is_err());
    }

    #[test]
    fn invalid_geometry_and_too_small_output_fail_before_capture() {
        for source in [
            region(-1, 0, 1920, 1080),
            region(0, -1, 1920, 1080),
            region(1, 0, 1920, 1080),
            region(0, 1, 1920, 1080),
            region(0, 0, 0, 1080),
            region(0, 0, u32::MAX, 1080),
        ] {
            assert!(validate_region(source, (1920, 1080)).is_err());
        }
        assert!(X11CapturePlan::new(region(0, 0, 1920, 1080), 1, 720).is_err());
        assert!(X11CapturePlan::new(region(0, 0, 1024, 2), 2, 2).is_err());
    }

    #[test]
    #[ignore = "requires FFmpeg; uses only generated raw pixels, never a screen or window"]
    fn ffmpeg_scale_preserves_all_four_source_quadrants_and_exact_output_size() {
        use std::io::{Read, Write};
        use std::process::Stdio;
        use std::time::{Duration, Instant};

        let colors = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]];
        for (source, requested, expected) in [
            ((3840u32, 2160u32), (1920, 1080), (1920, 1080)),
            ((2560, 1600), (1280, 720), (1152, 720)),
            ((1080, 1920), (1920, 1080), (606, 1080)),
            ((1280, 720), (1920, 1080), (1280, 720)),
            ((1919, 1079), (1920, 1080), (1918, 1078)),
        ] {
            let plan = X11CapturePlan::new(
                root_region(source.0, source.1).unwrap(),
                requested.0,
                requested.1,
            )
            .unwrap();
            let mut planned_command = Command::new("unspawned-capture-fixture");
            plan.append_args(&mut planned_command, ":0", 1);
            let args: Vec<_> = planned_command.get_args().collect();
            let filter = args.windows(2).find(|pair| pair[0] == "-vf").unwrap()[1];

            let mut pixels = Vec::with_capacity((source.0 * source.1 * 3) as usize);
            for y in 0..source.1 {
                for x in 0..source.0 {
                    let quadrant =
                        usize::from(x * 2 >= source.0) + 2 * usize::from(y * 2 >= source.1);
                    pixels.extend_from_slice(&colors[quadrant]);
                }
            }
            let mut child = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                    &format!("{}x{}", source.0, source.1),
                    "-i",
                    "pipe:0",
                    "-vf",
                ])
                .arg(filter)
                .args([
                    "-filter_threads",
                    "1",
                    "-threads",
                    "1",
                    "-frames:v",
                    "1",
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("install FFmpeg to run this explicit offline fixture test");
            let mut stdin = child.stdin.take().unwrap();
            let writer = std::thread::spawn(move || stdin.write_all(&pixels));
            let mut stdout = child.stdout.take().unwrap();
            let reader = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let mut stderr = child.stderr.take().unwrap();
            let errors = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).unwrap();
                bytes
            });
            let started = Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break status;
                }
                if started.elapsed() >= Duration::from_secs(10) {
                    let _ = child.kill();
                    break child.wait().unwrap();
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let write_result = writer.join().unwrap();
            let output = reader.join().unwrap();
            let error = errors.join().unwrap();
            assert!(status.success(), "{}", String::from_utf8_lossy(&error));
            write_result.unwrap();
            assert_eq!(output.len(), (expected.0 * expected.1 * 3) as usize);
            for (quadrant, (x, y)) in [
                (expected.0 / 4, expected.1 / 4),
                (expected.0 * 3 / 4, expected.1 / 4),
                (expected.0 / 4, expected.1 * 3 / 4),
                (expected.0 * 3 / 4, expected.1 * 3 / 4),
            ]
            .into_iter()
            .enumerate()
            {
                let offset = ((y * expected.0 + x) * 3) as usize;
                assert_eq!(
                    &output[offset..offset + 3],
                    &colors[quadrant],
                    "source {source:?}, output {expected:?}, quadrant {quadrant}"
                );
            }
            println!(
                "offline FFmpeg fixture: source {}x{}, requested {}x{}, output {}x{}, all quadrants retained",
                source.0, source.1, requested.0, requested.1, expected.0, expected.1
            );
        }
    }
}
