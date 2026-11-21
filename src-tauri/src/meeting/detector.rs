use crate::meeting::macos_bridge::{self, RunningCallApp};
use crate::meeting::types::{DetectedCallInfo, OutputRouteKind};

const POSITIVE_POLLS_REQUIRED: u8 = 2;
const INACTIVE_POLLS_TO_RESET: u8 = 3;

#[derive(Debug, Clone, Default)]
pub struct CallDetectionState {
    positive_polls: u8,
    inactive_mic_polls: u8,
    episode_started_at_ms: Option<i64>,
    episode_bundle_id: Option<String>,
    active_call: Option<DetectedCallInfo>,
    snoozed_call_id: Option<String>,
}

impl CallDetectionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_call_id(&self) -> Option<&str> {
        self.active_call.as_ref().map(|call| call.call_id.as_str())
    }

    pub fn snooze_call(&mut self, call_id: &str) {
        self.snoozed_call_id = Some(call_id.to_owned());
    }

    pub fn evaluate(
        &mut self,
        is_default_input_running: bool,
        own_app_using_mic: bool,
        running_call_app: Option<RunningCallApp>,
        output_route: OutputRouteKind,
        now_ms: i64,
    ) -> Option<DetectedCallInfo> {
        let can_detect =
            is_default_input_running && !own_app_using_mic && running_call_app.is_some();

        if can_detect {
            self.inactive_mic_polls = 0;
            let app = running_call_app.expect("checked above");

            let is_new_episode = self
                .episode_bundle_id
                .as_deref()
                .is_none_or(|bundle| bundle != app.bundle_id.as_str());
            if is_new_episode || self.active_call.is_none() {
                self.episode_bundle_id = Some(app.bundle_id.clone());
                self.episode_started_at_ms = Some(now_ms);
                self.positive_polls = 0;
            }

            self.positive_polls = self.positive_polls.saturating_add(1);
            let episode_epoch = self.episode_started_at_ms.unwrap_or(now_ms);
            let call_id = format!("{}:{episode_epoch}", app.bundle_id);

            self.active_call = Some(DetectedCallInfo {
                call_id: call_id.clone(),
                app_bundle_id: app.bundle_id,
                app_name: app.app_name,
                output_route,
                detected_at_ms: episode_epoch,
            });

            if self.positive_polls < POSITIVE_POLLS_REQUIRED {
                return None;
            }

            if self.snoozed_call_id.as_deref() == Some(call_id.as_str()) {
                return None;
            }

            return self.active_call.clone();
        }

        self.positive_polls = 0;
        if !is_default_input_running {
            self.inactive_mic_polls = self.inactive_mic_polls.saturating_add(1);
            if self.inactive_mic_polls >= INACTIVE_POLLS_TO_RESET {
                self.reset_episode();
            }
        } else {
            self.inactive_mic_polls = 0;
        }

        None
    }

    fn reset_episode(&mut self) {
        self.positive_polls = 0;
        self.inactive_mic_polls = 0;
        self.episode_started_at_ms = None;
        self.episode_bundle_id = None;
        self.active_call = None;
        self.snoozed_call_id = None;
    }
}

pub fn poll_call_detection(
    state: &mut CallDetectionState,
    own_app_using_mic: bool,
    now_ms: i64,
) -> Option<DetectedCallInfo> {
    state.evaluate(
        macos_bridge::is_default_input_running(),
        own_app_using_mic,
        macos_bridge::detect_running_call_app(),
        macos_bridge::detect_output_route(),
        now_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::macos_bridge::RunningCallAppTier;

    fn app(bundle_id: &str, app_name: &str) -> RunningCallApp {
        RunningCallApp {
            bundle_id: bundle_id.to_owned(),
            app_name: app_name.to_owned(),
            tier: RunningCallAppTier::Dedicated,
        }
    }

    #[test]
    fn requires_two_consecutive_positive_polls() {
        let mut state = CallDetectionState::new();
        let call_app = app("us.zoom.xos", "Zoom");

        let first = state.evaluate(
            true,
            false,
            Some(call_app.clone()),
            OutputRouteKind::Speakers,
            1_000,
        );
        assert!(first.is_none());

        let second = state.evaluate(
            true,
            false,
            Some(call_app),
            OutputRouteKind::Speakers,
            3_500,
        );
        let detected = second.expect("second positive poll should detect");
        assert_eq!(detected.app_bundle_id, "us.zoom.xos");
        assert_eq!(detected.call_id, "us.zoom.xos:1000");
    }

    #[test]
    fn own_app_mic_usage_suppresses_detection() {
        let mut state = CallDetectionState::new();
        let call_app = app("com.microsoft.teams2", "Teams");

        assert!(state
            .evaluate(
                true,
                true,
                Some(call_app.clone()),
                OutputRouteKind::Headphones,
                2_000
            )
            .is_none());
        assert!(state
            .evaluate(
                true,
                true,
                Some(call_app),
                OutputRouteKind::Headphones,
                4_500
            )
            .is_none());
        assert!(state.active_call_id().is_none());
    }

    #[test]
    fn snooze_sticks_for_current_call_episode() {
        let mut state = CallDetectionState::new();
        let call_app = app("com.apple.FaceTime", "FaceTime");

        assert!(state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Headphones,
                10_000
            )
            .is_none());
        let detected = state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Headphones,
                12_500,
            )
            .expect("call should be detected on second poll");
        state.snooze_call(&detected.call_id);

        assert!(state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Headphones,
                15_000
            )
            .is_none());
        assert!(state
            .evaluate(
                true,
                false,
                Some(call_app),
                OutputRouteKind::Headphones,
                17_500
            )
            .is_none());
    }

    #[test]
    fn reset_after_three_inactive_polls_allows_new_episode_after_snooze() {
        let mut state = CallDetectionState::new();
        let call_app = app("us.zoom.xos", "Zoom");

        assert!(state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Speakers,
                1_000
            )
            .is_none());
        let detected = state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Speakers,
                3_500,
            )
            .expect("call should be detected");
        state.snooze_call(&detected.call_id);

        assert!(state
            .evaluate(false, false, None, OutputRouteKind::Unknown, 6_000)
            .is_none());
        assert!(state
            .evaluate(false, false, None, OutputRouteKind::Unknown, 8_500)
            .is_none());
        assert!(state
            .evaluate(false, false, None, OutputRouteKind::Unknown, 11_000)
            .is_none());
        assert!(state.active_call_id().is_none());

        assert!(state
            .evaluate(
                true,
                false,
                Some(call_app.clone()),
                OutputRouteKind::Speakers,
                13_500
            )
            .is_none());
        let resumed = state
            .evaluate(
                true,
                false,
                Some(call_app),
                OutputRouteKind::Speakers,
                16_000,
            )
            .expect("new episode should detect again");
        assert_eq!(resumed.call_id, "us.zoom.xos:13500");
    }
}
