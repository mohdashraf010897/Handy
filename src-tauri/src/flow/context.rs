//! Frontmost-app capture at hotkey press.
//!
//! Only the app identity is read (macOS bundle id and name, Windows exe name);
//! window titles are never read because they routinely contain private data.

use std::time::Instant;

/// The app that was frontmost when dictation started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppContext {
    /// Bundle id on macOS (`com.tinyspeck.slackmacgap`), exe name on Windows
    /// (`slack.exe`).
    pub app_id: Option<String>,
    /// Display name, for diagnostics only.
    pub app_name: Option<String>,
}

/// Captures the frontmost app and how long the capture took, in microseconds.
pub fn capture_timed() -> (Option<AppContext>, u64) {
    let started = Instant::now();
    let context = capture();
    let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    (context, micros)
}

#[cfg(target_os = "macos")]
fn capture() -> Option<AppContext> {
    use objc2_app_kit::NSWorkspace;

    objc2::rc::autoreleasepool(|_| {
        let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
        let context = AppContext {
            app_id: app.bundleIdentifier().map(|id| id.to_string()),
            app_name: app.localizedName().map(|name| name.to_string()),
        };
        non_empty(context)
    })
}

#[cfg(target_os = "windows")]
fn capture() -> Option<AppContext> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    // SAFETY: GetForegroundWindow takes no arguments and may return null.
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `window` is a window handle and `pid` outlives the call.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid as *mut u32)) };
    if pid == 0 {
        return None;
    }
    // SAFETY: limited query access needs no privileges; failure is an Err.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `buffer` holds `len` u16s and `process` is an open handle.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: `process` was opened above and is closed exactly once.
    if let Err(e) = unsafe { CloseHandle(process) } {
        log::debug!("flow: CloseHandle failed: {e}");
    }
    queried.ok()?;
    let path = String::from_utf16_lossy(buffer.get(..len as usize)?);
    let exe = std::path::Path::new(&path)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    non_empty(AppContext {
        app_id: Some(exe.clone()),
        app_name: Some(exe),
    })
}

/// Linux has no portable frontmost-app API (X11 vs. Wayland compositors).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn capture() -> Option<AppContext> {
    None
}

#[cfg_attr(
    all(not(test), not(any(target_os = "macos", target_os = "windows"))),
    expect(dead_code, reason = "Linux capture always returns None")
)]
fn non_empty(context: AppContext) -> Option<AppContext> {
    let has = |value: &Option<String>| value.as_deref().is_some_and(|v| !v.trim().is_empty());
    (has(&context.app_id) || has(&context.app_name)).then_some(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_context_is_treated_as_not_captured() {
        assert_eq!(non_empty(AppContext::default()), None);
        let blank = AppContext {
            app_id: Some("  ".into()),
            app_name: None,
        };
        assert_eq!(non_empty(blank), None);
        let named = AppContext {
            app_id: None,
            app_name: Some("Slack".into()),
        };
        assert!(non_empty(named).is_some());
    }

    #[test]
    fn capture_is_fast_and_does_not_panic() {
        let (_, micros) = capture_timed();
        assert!(micros < 1_000_000, "capture took {micros} us");
    }
}
