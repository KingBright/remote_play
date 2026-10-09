//! Describe the command's actual input, without inferring capture support from
//! desktop environment variables. This is a log label, not a readiness check.
use std::process::Command;

pub(crate) fn ffmpeg_input_format(command: &Command) -> &'static str {
    let mut args = command.get_args();
    let mut format = None;
    while let Some(arg) = args.next() {
        if arg == "-i" {
            return match format.and_then(|value: &std::ffi::OsStr| value.to_str()) {
                Some("x11grab") => "x11grab",
                Some("kmsgrab") => "kmsgrab",
                Some("gdigrab") => "gdigrab",
                _ => "unknown",
            };
        }
        if arg == "-f" {
            format = args.next();
        }
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_environment_does_not_turn_x11_input_into_pipewire() {
        let mut command = Command::new("fixture-ffmpeg");
        command.env("WAYLAND_DISPLAY", "wayland-0");
        command.args(["-f", "x11grab", "-i", ":0", "-f", "hevc", "pipe:1"]);
        assert_eq!(ffmpeg_input_format(&command), "x11grab");
    }

    #[test]
    fn actual_input_formats_are_kept_separate_from_encoded_output() {
        for format in ["x11grab", "kmsgrab", "gdigrab"] {
            let mut command = Command::new("fixture-ffmpeg");
            command.args(["-f", format, "-i", "owned-input", "-f", "hevc", "pipe:1"]);
            assert_eq!(ffmpeg_input_format(&command), format);
        }
    }

    #[test]
    fn missing_or_unimplemented_input_never_claims_a_capture_backend() {
        for arguments in [
            vec!["-f", "hevc", "pipe:1"],
            vec!["-i", "owned-input", "-f", "x11grab", "pipe:1"],
            vec!["-f", "pipewire", "-i", "owned-input"],
            vec!["-f", "unknown", "-i", "owned-input"],
        ] {
            let mut command = Command::new("fixture-ffmpeg");
            command.args(arguments);
            assert_eq!(ffmpeg_input_format(&command), "unknown");
        }
    }
}
