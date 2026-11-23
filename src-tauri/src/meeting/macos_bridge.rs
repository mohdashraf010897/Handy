use crate::meeting::types::OutputRouteKind;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CalendarAccessStatus {
    NotDetermined,
    Authorized,
    Denied,
    Unsupported,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ActiveCalendarMeeting {
    pub title: String,
    pub attendees: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningCallAppTier {
    Dedicated,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningCallApp {
    pub bundle_id: String,
    pub app_name: String,
    pub tier: RunningCallAppTier,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{
        ActiveCalendarMeeting, CalendarAccessStatus, OutputRouteKind, RunningCallApp,
        RunningCallAppTier,
    };
    use std::ffi::{c_void, CStr};
    use std::os::raw::c_char;

    const CALL_APP_BUFFER_CAP: usize = 256;
    const CALENDAR_JSON_BUFFER_CAP: usize = 4096;

    unsafe extern "C" {
        fn lowyap_is_default_input_running() -> i32;
        fn lowyap_detect_output_route() -> i32;
        fn lowyap_detect_running_call_app(
            out_bundle: *mut c_char,
            bundle_cap: i32,
            out_name: *mut c_char,
            name_cap: i32,
        ) -> i32;
        fn lowyap_system_tap_start() -> *mut c_void;
        fn lowyap_system_tap_drain(
            handle: *mut c_void,
            out_samples: *mut f32,
            max_samples: i32,
        ) -> i32;
        fn lowyap_system_tap_stop(handle: *mut c_void);
        fn lowyap_calendar_auth_status() -> i32;
        fn lowyap_request_calendar_access() -> i32;
        fn lowyap_active_calendar_meeting_json(out_json: *mut c_char, cap: i32) -> i32;
    }

    pub fn calendar_access_status() -> CalendarAccessStatus {
        match unsafe { lowyap_calendar_auth_status() } {
            0 => CalendarAccessStatus::NotDetermined,
            1 => CalendarAccessStatus::Authorized,
            _ => CalendarAccessStatus::Denied,
        }
    }

    pub fn request_calendar_access() -> CalendarAccessStatus {
        match unsafe { lowyap_request_calendar_access() } {
            0 => CalendarAccessStatus::NotDetermined,
            1 => CalendarAccessStatus::Authorized,
            _ => CalendarAccessStatus::Denied,
        }
    }

    pub fn active_calendar_meeting() -> Option<ActiveCalendarMeeting> {
        let mut buf = [0 as c_char; CALENDAR_JSON_BUFFER_CAP];
        let found = unsafe {
            lowyap_active_calendar_meeting_json(buf.as_mut_ptr(), CALENDAR_JSON_BUFFER_CAP as i32)
        };
        if found != 1 {
            return None;
        }
        let raw = unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_owned();
        if raw.is_empty() {
            return None;
        }
        serde_json::from_str::<ActiveCalendarMeeting>(&raw)
            .ok()
            .filter(|m| !m.title.trim().is_empty())
    }

    pub fn is_default_input_running() -> bool {
        unsafe { lowyap_is_default_input_running() == 1 }
    }

    pub fn detect_output_route() -> OutputRouteKind {
        match unsafe { lowyap_detect_output_route() } {
            1 => OutputRouteKind::Headphones,
            2 => OutputRouteKind::Speakers,
            _ => OutputRouteKind::Unknown,
        }
    }

    pub fn detect_running_call_app() -> Option<RunningCallApp> {
        let mut bundle = [0 as c_char; CALL_APP_BUFFER_CAP];
        let mut name = [0 as c_char; CALL_APP_BUFFER_CAP];
        let tier = unsafe {
            lowyap_detect_running_call_app(
                bundle.as_mut_ptr(),
                CALL_APP_BUFFER_CAP as i32,
                name.as_mut_ptr(),
                CALL_APP_BUFFER_CAP as i32,
            )
        };
        let tier = match tier {
            1 => RunningCallAppTier::Dedicated,
            2 => RunningCallAppTier::Browser,
            _ => return None,
        };

        let bundle_id = unsafe { CStr::from_ptr(bundle.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_owned();
        if bundle_id.is_empty() {
            return None;
        }
        let app_name = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .trim()
            .to_owned();

        Some(RunningCallApp {
            bundle_id: bundle_id.clone(),
            app_name: if app_name.is_empty() {
                bundle_id
            } else {
                app_name
            },
            tier,
        })
    }

    #[derive(Debug)]
    pub struct SystemAudioTap {
        handle: *mut c_void,
    }

    impl SystemAudioTap {
        pub fn start() -> Option<Self> {
            let handle = unsafe { lowyap_system_tap_start() };
            (!handle.is_null()).then_some(Self { handle })
        }

        pub fn drain(&mut self, max_samples: usize) -> Vec<f32> {
            if max_samples == 0 || self.handle.is_null() {
                return Vec::new();
            }
            let mut out = vec![0.0_f32; max_samples];
            let drained = unsafe {
                lowyap_system_tap_drain(self.handle, out.as_mut_ptr(), max_samples as i32)
            };
            if drained <= 0 {
                return Vec::new();
            }
            out.truncate(drained as usize);
            out
        }

        pub fn stop(mut self) {
            self.stop_inner();
        }

        fn stop_inner(&mut self) {
            if self.handle.is_null() {
                return;
            }
            unsafe { lowyap_system_tap_stop(self.handle) };
            self.handle = std::ptr::null_mut();
        }
    }

    impl Drop for SystemAudioTap {
        fn drop(&mut self) {
            self.stop_inner();
        }
    }

    unsafe impl Send for SystemAudioTap {}
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::{ActiveCalendarMeeting, CalendarAccessStatus, OutputRouteKind, RunningCallApp};

    pub fn calendar_access_status() -> CalendarAccessStatus {
        CalendarAccessStatus::Unsupported
    }

    pub fn request_calendar_access() -> CalendarAccessStatus {
        CalendarAccessStatus::Unsupported
    }

    pub fn active_calendar_meeting() -> Option<ActiveCalendarMeeting> {
        None
    }

    pub fn is_default_input_running() -> bool {
        false
    }

    pub fn detect_output_route() -> OutputRouteKind {
        OutputRouteKind::Unknown
    }

    pub fn detect_running_call_app() -> Option<RunningCallApp> {
        None
    }

    #[derive(Debug)]
    pub struct SystemAudioTap;

    impl SystemAudioTap {
        pub fn start() -> Option<Self> {
            None
        }

        pub fn drain(&mut self, _max_samples: usize) -> Vec<f32> {
            Vec::new()
        }

        pub fn stop(self) {}
    }
}

pub use platform::{
    active_calendar_meeting, calendar_access_status, detect_output_route, detect_running_call_app,
    is_default_input_running, request_calendar_access, SystemAudioTap,
};
