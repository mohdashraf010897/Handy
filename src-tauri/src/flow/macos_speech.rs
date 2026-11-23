//! Batch dictation through Apple's on-device recognizer; never a network fallback.

use anyhow::Result;

#[cfg(any(target_os = "macos", macos_speech_swift_stub))]
use std::ffi::{c_char, CStr, CString};

pub const MACOS_NATIVE_SPEECH_MODEL_ID: &str = "macos-native-speech";

/// Resolve a single language without detecting speech or escaping the spoken
/// language preference. `None` in the success case means the system locale.
#[cfg(target_os = "macos")]
pub(crate) fn resolve_locale<'a>(
    selected: &'a str,
    spoken: &'a [String],
    available: impl Fn(Option<&str>) -> bool,
) -> Result<Option<&'a str>> {
    let concrete = |s: &str| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case("auto");
    let selected = selected.trim();
    let requested = if concrete(selected) {
        Some(selected)
    } else {
        spoken.iter().find(|s| concrete(s)).map(|s| s.trim())
    };
    if available(requested) {
        return Ok(requested);
    }
    let english = |s: &str| {
        s.trim()
            .split(['-', '_'])
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case("en"))
    };
    let english_allowed = requested.is_none()
        || english(selected)
        || spoken
            .iter()
            .any(|s| english(s) || s.trim().eq_ignore_ascii_case("auto"));
    if english_allowed && requested != Some("en-US") && available(Some("en-US")) {
        return Ok(Some("en-US"));
    }
    anyhow::bail!("On-device macOS Speech Recognition is unavailable for this language. Check your macOS speech language settings or choose a downloaded model.")
}

#[cfg(any(target_os = "macos", macos_speech_swift_stub))]
unsafe extern "C" {
    fn lowyap_macos_speech_available(locale: *const c_char) -> bool;
    fn lowyap_macos_speech_transcribe_f32(
        samples: *const f32,
        sample_count: isize,
        locale: *const c_char,
        out_error: *mut *mut c_char,
    ) -> *mut c_char;
    fn lowyap_macos_speech_free_string(text: *mut c_char);
}

/// Whether the requested or system locale is ready for on-device recognition.
pub fn is_available(locale: Option<&str>) -> bool {
    #[cfg(any(target_os = "macos", macos_speech_swift_stub))]
    {
        let Ok(locale) = locale.map(CString::new).transpose() else {
            return false;
        };
        // SAFETY: The locale is null or NUL-terminated and lives through the call.
        unsafe {
            lowyap_macos_speech_available(locale.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()))
        }
    }
    #[cfg(not(any(target_os = "macos", macos_speech_swift_stub)))]
    {
        let _ = locale;
        false
    }
}

/// Transcribe mono float PCM at 16 kHz using the supplied or system locale.
/// Calls with speech must run off the main thread to allow the permission prompt.
pub fn transcribe_pcm_16k(samples: &[f32], locale: Option<&str>) -> Result<String> {
    if samples.is_empty() || (cfg!(not(target_os = "macos")) && samples.iter().all(|s| *s == 0.0)) {
        return Ok(String::new());
    }
    #[cfg(any(target_os = "macos", macos_speech_swift_stub))]
    {
        let locale = locale
            .map(CString::new)
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid on-device speech locale."))?;
        let mut error = std::ptr::null_mut();
        // SAFETY: The slice outlives this synchronous call, its byte length
        // fits isize, and the bridge borrows the locale and error slot only.
        let text = unsafe {
            lowyap_macos_speech_transcribe_f32(
                samples.as_ptr(),
                samples.len() as isize,
                locale.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                &mut error,
            )
        };
        let text = take_bridge_string(text);
        if let Some(error) = take_bridge_string(error) {
            anyhow::bail!("{error}");
        }
        text.ok_or_else(|| {
            anyhow::anyhow!("On-device macOS Speech Recognition did not return a result.")
        })
    }
    #[cfg(not(any(target_os = "macos", macos_speech_swift_stub)))]
    {
        let _ = locale;
        anyhow::bail!("On-device macOS Speech Recognition is not available on this platform.")
    }
}

#[cfg(any(target_os = "macos", macos_speech_swift_stub))]
fn take_bridge_string(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: The bridge returns owned, NUL-terminated strings allocated
    // with strdup and exposes the matching deallocator.
    unsafe {
        let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        lowyap_macos_speech_free_string(ptr);
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_pcm_returns_empty_text() {
        assert_eq!(transcribe_pcm_16k(&[], None).unwrap(), "");
    }

    #[test]
    fn silent_pcm_returns_empty_text_without_permission_or_a_pack() {
        assert_eq!(
            transcribe_pcm_16k(&[0.0; 16_000], Some("not-a-real-locale")).unwrap(),
            ""
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unavailable_locale_fails_before_requesting_permission() {
        let error = transcribe_pcm_16k(&[0.5; 16_000], Some("und-ZZ")).unwrap_err();
        assert!(error.to_string().contains("unavailable for this language"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_non_english_pack_does_not_try_english_without_permission() {
        assert!(resolve_locale("ja", &[], |locale| {
            assert_eq!(locale, Some("ja"));
            false
        })
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_pack_can_try_english_when_listed_as_spoken() {
        let spoken = ["ja".to_string(), "en-GB".to_string()];
        assert_eq!(
            resolve_locale("auto", &spoken, |locale| locale == Some("en-US")).unwrap(),
            Some("en-US")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn auto_respects_an_explicit_non_english_spoken_list() {
        let spoken = ["fr".to_string()];
        assert!(resolve_locale("auto", &spoken, |locale| {
            assert_eq!(locale, Some("fr"));
            false
        })
        .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn auto_without_spoken_preferences_uses_english_when_system_pack_is_missing() {
        assert_eq!(
            resolve_locale("auto", &[], |locale| locale == Some("en-US")).unwrap(),
            Some("en-US")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn platform_without_native_recognition_fails_locally() {
        assert!(transcribe_pcm_16k(&[1.0], None).is_err());
    }
}
