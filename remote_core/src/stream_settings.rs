//! Stream form state and effects; execution and persistence stay in the adapter.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSettingsValues {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}
impl StreamSettingsValues {
    pub fn resolution(self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsReceipt {
    command: u64,
    view: u64,
}

pub enum StreamSettingsAction {
    EditFps(String),
    EditBitrate(String),
    ApplyCustom,
    SelectResolution(u32, u32),
    SelectFps(u32),
    SelectBitrate(u32),
    UpdateFailed(SettingsReceipt, String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdatePolicy {
    Always,
    WhenConnected,
}

/// A commit requests the existing preferences write and optional Owner update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSettingsEffect {
    pub values: StreamSettingsValues,
    pub update: UpdatePolicy,
    pub report_error: bool,
    pub receipt: SettingsReceipt,
}
impl StreamSettingsEffect {
    pub fn should_update(self, connected: bool) -> bool {
        self.update == UpdatePolicy::Always || connected
    }
}

pub struct StreamSettingsViewModel<'a> {
    pub values: StreamSettingsValues,
    pub fps_text: &'a str,
    pub bitrate_text: &'a str,
    pub error: Option<&'a str>,
}

pub struct StreamSettingsState {
    values: StreamSettingsValues,
    fps_text: String,
    bitrate_text: String,
    error: Option<String>,
    command: u64,
    view: u64,
}
impl StreamSettingsState {
    pub fn new(values: StreamSettingsValues) -> Self {
        Self {
            values,
            fps_text: values.fps.to_string(),
            bitrate_text: values.bitrate_kbps.to_string(),
            error: None,
            command: 0,
            view: 0,
        }
    }
    pub fn values(&self) -> StreamSettingsValues {
        self.values
    }
    pub fn project(&self) -> StreamSettingsViewModel<'_> {
        StreamSettingsViewModel {
            values: self.values,
            fps_text: &self.fps_text,
            bitrate_text: &self.bitrate_text,
            error: self.error.as_deref(),
        }
    }
    pub fn effect_is_current(&self, receipt: SettingsReceipt) -> bool {
        self.command == receipt.command
    }
    pub fn reduce(&mut self, action: StreamSettingsAction) -> Option<StreamSettingsEffect> {
        if let StreamSettingsAction::UpdateFailed(receipt, error) = action {
            if self.effect_is_current(receipt) && self.view == receipt.view {
                self.error = Some(error);
            }
            return None;
        }
        self.view = self.view.wrapping_add(1);
        let custom = matches!(action, StreamSettingsAction::ApplyCustom);
        match action {
            StreamSettingsAction::EditFps(text) => {
                self.fps_text = text;
                return None;
            }
            StreamSettingsAction::EditBitrate(text) => {
                self.bitrate_text = text;
                return None;
            }
            StreamSettingsAction::ApplyCustom => {
                let parsed = self
                    .fps_text
                    .trim()
                    .parse::<u32>()
                    .ok()
                    .zip(self.bitrate_text.trim().parse::<u32>().ok());
                let Some((fps, bitrate_kbps)) = parsed.filter(|&(fps, bitrate)| {
                    protocol::validate_video_settings(
                        self.values.width,
                        self.values.height,
                        fps,
                        bitrate,
                    )
                    .is_ok()
                }) else {
                    self.error = Some("Enter positive whole numbers for FPS and kbps.".into());
                    return None;
                };
                self.values.fps = fps;
                self.values.bitrate_kbps = bitrate_kbps;
                self.error = None;
            }
            StreamSettingsAction::SelectResolution(width, height) => {
                self.values.width = width;
                self.values.height = height;
            }
            StreamSettingsAction::SelectFps(fps) => {
                self.values.fps = fps;
                self.fps_text = fps.to_string();
            }
            StreamSettingsAction::SelectBitrate(bitrate) => {
                self.values.bitrate_kbps = bitrate;
                self.bitrate_text = bitrate.to_string();
            }
            StreamSettingsAction::UpdateFailed(_, _) => unreachable!(),
        }
        self.command = self.command.wrapping_add(1);
        Some(StreamSettingsEffect {
            values: self.values,
            update: if custom {
                UpdatePolicy::WhenConnected
            } else {
                UpdatePolicy::Always
            },
            report_error: custom,
            receipt: SettingsReceipt {
                command: self.command,
                view: self.view,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn values() -> StreamSettingsValues {
        StreamSettingsValues {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 10_000,
        }
    }
    fn state() -> StreamSettingsState {
        StreamSettingsState::new(values())
    }

    #[test]
    fn drafts_survive_projection_without_changing_committed_values_or_emitting_effects() {
        let mut state = state();
        assert!(
            state
                .reduce(StreamSettingsAction::EditFps(" 75 ".into()))
                .is_none()
        );
        assert!(
            state
                .reduce(StreamSettingsAction::EditBitrate("12345".into()))
                .is_none()
        );
        let view = state.project();
        assert_eq!(view.fps_text, " 75 ");
        assert_eq!(view.bitrate_text, "12345");
        assert_eq!(view.values, values());
        assert_eq!(view.error, None);
        let effect = state.reduce(StreamSettingsAction::ApplyCustom).unwrap();
        assert_eq!(effect.values.fps, 75);
        assert_eq!(effect.values.bitrate_kbps, 12345);
        assert!(!effect.should_update(false));
        assert!(effect.should_update(true));
        assert!(effect.report_error);
    }
    #[test]
    fn invalid_custom_values_preserve_committed_settings_and_have_no_side_effects() {
        for text in ["", "0", "-1", "1.5", "4294967296", "214748365"] {
            let mut state = state();
            state.reduce(StreamSettingsAction::EditFps(text.into()));
            assert!(
                state.reduce(StreamSettingsAction::ApplyCustom).is_none(),
                "{text}"
            );
            assert_eq!(state.values(), values());
            assert!(state.project().error.is_some());
        }
        let mut state = state();
        state.reduce(StreamSettingsAction::EditBitrate("0".into()));
        assert!(state.reduce(StreamSettingsAction::ApplyCustom).is_none());
        assert_eq!(state.values(), values());
        state.reduce(StreamSettingsAction::EditBitrate(u32::MAX.to_string()));
        assert_eq!(
            state
                .reduce(StreamSettingsAction::ApplyCustom)
                .unwrap()
                .values
                .bitrate_kbps,
            u32::MAX
        );
    }
    #[test]
    fn presets_preserve_other_values_and_sync_only_the_matching_draft() {
        let mut state = state();
        state.reduce(StreamSettingsAction::EditFps("75".into()));
        let resolution = state
            .reduce(StreamSettingsAction::SelectResolution(3840, 2160))
            .unwrap();
        assert_eq!(resolution.values.resolution(), (3840, 2160));
        assert_eq!(resolution.values.fps, 60);
        assert_eq!(state.project().fps_text, "75");
        let fps = state.reduce(StreamSettingsAction::SelectFps(120)).unwrap();
        assert_eq!(state.project().fps_text, "120");
        assert_eq!(fps.values.bitrate_kbps, 10_000);
        let bitrate = state
            .reduce(StreamSettingsAction::SelectBitrate(80_000))
            .unwrap();
        assert_eq!(state.project().bitrate_text, "80000");
        assert_eq!(bitrate.values.fps, 120);
        assert!(bitrate.should_update(false));
        assert!(!bitrate.report_error);
    }
    #[test]
    fn editing_next_draft_keeps_committed_effect_valid_but_rejects_its_late_error() {
        let mut state = state();
        let effect = state.reduce(StreamSettingsAction::ApplyCustom).unwrap();
        state.reduce(StreamSettingsAction::EditFps("75".into()));
        assert!(state.effect_is_current(effect.receipt));
        state.reduce(StreamSettingsAction::UpdateFailed(
            effect.receipt,
            "old error".into(),
        ));
        assert_eq!(state.project().error, None);
        assert_eq!(state.project().fps_text, "75");
        let latest = state.reduce(StreamSettingsAction::ApplyCustom).unwrap();
        assert!(!state.effect_is_current(effect.receipt));
        state.reduce(StreamSettingsAction::UpdateFailed(
            latest.receipt,
            "current error".into(),
        ));
        assert_eq!(state.project().error, Some("current error"));
        state.reduce(StreamSettingsAction::EditFps("bad".into()));
        state.reduce(StreamSettingsAction::ApplyCustom);
        state.reduce(StreamSettingsAction::UpdateFailed(
            latest.receipt,
            "late error".into(),
        ));
        assert_eq!(
            state.project().error,
            Some("Enter positive whole numbers for FPS and kbps.")
        );
    }
}
