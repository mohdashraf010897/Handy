//! Best-effort, permission-gated cursor context. No text or application identity
//! is logged; password controls and unsupported values are never captured.

use lowyap_ports::{AppSurfaceKind, FieldContextProvider, FieldContextSnapshot};

use super::context::AppContext;

pub struct PlatformFieldContextProvider {
    surface: AppSurfaceKind,
}

impl PlatformFieldContextProvider {
    /// Uses only the press-time app identifier, never the window title.
    pub fn new(app: Option<&AppContext>) -> Self {
        Self {
            surface: classify_surface(app.and_then(|app| app.app_id.as_deref())),
        }
    }
}

impl Default for PlatformFieldContextProvider {
    fn default() -> Self {
        Self::new(None)
    }
}

impl FieldContextProvider for PlatformFieldContextProvider {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macos_ax"
        } else {
            "surface_only"
        }
    }

    fn capture_field_context(&self) -> Option<FieldContextSnapshot> {
        #[cfg(target_os = "macos")]
        {
            macos::capture(self.surface)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(FieldContextSnapshot {
                surface: self.surface,
                ..Default::default()
            })
        }
    }
}

fn classify_surface(app_id: Option<&str>) -> AppSurfaceKind {
    let Some(app_id) = app_id else {
        return AppSurfaceKind::General;
    };
    let id = app_id.to_ascii_lowercase();
    let matches = |ids: &[&str]| {
        ids.iter().any(|candidate| {
            id == *candidate
                || id
                    .strip_prefix(candidate)
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    };
    if matches(&[
        "com.tinyspeck.slackmacgap",
        "com.microsoft.teams",
        "com.microsoft.teams2",
        "com.hnc.discord",
        "net.whatsapp.whatsapp",
        "org.telegram.desktop",
        "org.whispersystems.signal-desktop",
        "com.apple.mobilesms",
    ]) {
        AppSurfaceKind::Chat
    } else if matches(&[
        "com.apple.mail",
        "com.microsoft.outlook",
        "com.apple.notes",
        "com.apple.iwork.pages",
        "com.microsoft.word",
        "notion.id",
        "md.obsidian",
        "com.evernote.evernote",
    ]) {
        AppSurfaceKind::EmailOrDoc
    } else if matches(&[
        "com.microsoft.vscode",
        "com.todesktop.230313mzl4w4u92",
        "com.exafunction.windsurf",
        "com.apple.dt.xcode",
        "com.jetbrains",
        "com.apple.terminal",
        "com.googlecode.iterm2",
        "dev.warp.warp-stable",
        "com.mitchellh.ghostty",
        "org.alacritty",
        "net.kovidgoyal.kitty",
    ]) {
        AppSurfaceKind::CodeOrTerminal
    } else if matches(&[
        "com.apple.safari",
        "org.mozilla.firefox",
        "com.google.chrome",
        "org.chromium.chromium",
        "com.microsoft.edgemac",
        "com.brave.browser",
        "com.operasoftware.opera",
        "company.thebrowser.browser",
    ]) {
        AppSurfaceKind::Browser
    } else {
        AppSurfaceKind::General
    }
}

#[cfg(any(target_os = "macos", test))]
fn supports_field(role: &str, subrole: Option<&str>) -> bool {
    matches!(role, "AXTextField" | "AXTextArea" | "AXComboBox")
        && subrole != Some("AXSecureTextField")
        && (role != "AXTextField" || subrole.is_some())
}

#[cfg(any(target_os = "macos", test))]
fn utf16_windows(
    total: usize,
    selection: std::ops::Range<usize>,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    if selection.start > selection.end || selection.end > total {
        return None;
    }
    Some((
        selection.start.saturating_sub(320)..selection.start,
        selection.end..selection.end.saturating_add(160).min(total),
    ))
}

#[cfg(any(target_os = "macos", test))]
fn bounded_utf16_edges(before: &[u16], after: &[u16]) -> Option<(String, String)> {
    // An AX selection inside a surrogate pair is not a valid cursor boundary.
    if before
        .last()
        .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        || after
            .first()
            .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
    {
        return None;
    }
    let before = if before
        .first()
        .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
    {
        &before[1..]
    } else {
        before
    };
    let after = if after
        .last()
        .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
    {
        &after[..after.len() - 1]
    } else {
        after
    };
    let before = String::from_utf16(before).ok()?;
    let after = String::from_utf16(after).ok()?;
    let before: String = before
        .chars()
        .rev()
        .take(160)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let after = after.chars().take(80).collect();
    Some((before, after))
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::time::{Duration, Instant};

    use super::*;

    type CfRef = *const c_void;
    const BUDGET: Duration = Duration::from_millis(3);
    const UTF8: u32 = 0x0800_0100;
    const AX_CF_RANGE: u32 = 4;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CfRange {
        location: isize,
        length: isize,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateSystemWide() -> CfRef;
        fn AXUIElementGetTypeID() -> usize;
        fn AXUIElementSetMessagingTimeout(element: CfRef, timeout: f32) -> i32;
        fn AXUIElementCopyAttributeValue(
            element: CfRef,
            attribute: CfRef,
            value: *mut CfRef,
        ) -> i32;
        fn AXValueGetTypeID() -> usize;
        fn AXValueGetValue(value: CfRef, kind: u32, output: *mut c_void) -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CfRef);
        fn CFGetTypeID(value: CfRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringCreateWithBytes(
            allocator: CfRef,
            bytes: *const u8,
            count: isize,
            encoding: u32,
            external: u8,
        ) -> CfRef;
        fn CFStringGetLength(value: CfRef) -> isize;
        fn CFStringGetCharacters(value: CfRef, range: CfRange, output: *mut u16);
    }

    struct OwnedCf(NonNull<c_void>);

    impl OwnedCf {
        unsafe fn from_created(value: CfRef) -> Option<Self> {
            NonNull::new(value.cast_mut()).map(Self)
        }

        fn as_ptr(&self) -> CfRef {
            self.0.as_ptr()
        }
    }

    impl Drop for OwnedCf {
        fn drop(&mut self) {
            // SAFETY: each OwnedCf owns exactly one Create/Copy reference.
            unsafe { CFRelease(self.as_ptr()) };
        }
    }

    fn copy_attribute(element: CfRef, name: &str, started: Instant) -> Option<OwnedCf> {
        let remaining = BUDGET.checked_sub(started.elapsed())?;
        if remaining.is_zero() {
            return None;
        }
        // SAFETY: element is a retained AXUIElement; names are valid UTF-8
        // and the out pointer is initialized before the Copy call.
        unsafe {
            if AXUIElementSetMessagingTimeout(element, remaining.as_secs_f32()) != 0 {
                return None;
            }
            let name = OwnedCf::from_created(CFStringCreateWithBytes(
                ptr::null(),
                name.as_ptr(),
                name.len() as isize,
                UTF8,
                0,
            ))?;
            let mut value = ptr::null();
            let status = AXUIElementCopyAttributeValue(element, name.as_ptr(), &mut value);
            let value = OwnedCf::from_created(value);
            if status != 0 || started.elapsed() >= BUDGET {
                return None;
            }
            value
        }
    }

    fn short_string(value: &OwnedCf) -> Option<String> {
        // SAFETY: validate CF's runtime type and length before reading characters.
        unsafe {
            if CFGetTypeID(value.as_ptr()) != CFStringGetTypeID() {
                return None;
            }
            let length = CFStringGetLength(value.as_ptr());
            if !(0..=80).contains(&length) {
                return None;
            }
            let mut units = vec![0; length as usize];
            CFStringGetCharacters(
                value.as_ptr(),
                CfRange {
                    location: 0,
                    length,
                },
                units.as_mut_ptr(),
            );
            String::from_utf16(&units).ok()
        }
    }

    struct SystemTimeoutReset(CfRef);

    impl Drop for SystemTimeoutReset {
        fn drop(&mut self) {
            // SAFETY: restores the process-wide default messaging timeout (0.0)
            // before `system` is released.
            unsafe {
                let _ = AXUIElementSetMessagingTimeout(self.0, 0.0);
            }
        }
    }

    pub(super) fn capture(surface: AppSurfaceKind) -> Option<FieldContextSnapshot> {
        let started = Instant::now();
        // SAFETY: trust is checked without prompting. Every subsequent AX/CF
        // reference is owned, and every out value's runtime type is checked.
        unsafe {
            if AXIsProcessTrusted() == 0 {
                return None;
            }
            let system = OwnedCf::from_created(AXUIElementCreateSystemWide())?;
            let _reset_system_timeout = SystemTimeoutReset(system.as_ptr());
            let field = copy_attribute(system.as_ptr(), "AXFocusedUIElement", started)?;
            if CFGetTypeID(field.as_ptr()) != AXUIElementGetTypeID() {
                return None;
            }
            let role = short_string(&copy_attribute(field.as_ptr(), "AXRole", started)?)?;
            let subrole = copy_attribute(field.as_ptr(), "AXSubrole", started)
                .and_then(|value| short_string(&value));
            if !supports_field(&role, subrole.as_deref()) {
                return None;
            }
            let selection = copy_attribute(field.as_ptr(), "AXSelectedTextRange", started)?;
            if CFGetTypeID(selection.as_ptr()) != AXValueGetTypeID() {
                return None;
            }
            let mut selected = CfRange {
                location: 0,
                length: 0,
            };
            if AXValueGetValue(
                selection.as_ptr(),
                AX_CF_RANGE,
                (&mut selected as *mut CfRange).cast(),
            ) == 0
            {
                return None;
            }
            let value = copy_attribute(field.as_ptr(), "AXValue", started)?;
            if CFGetTypeID(value.as_ptr()) != CFStringGetTypeID() {
                return None;
            }
            let total = usize::try_from(CFStringGetLength(value.as_ptr())).ok()?;
            let start = usize::try_from(selected.location).ok()?;
            let length = usize::try_from(selected.length).ok()?;
            let end = start.checked_add(length)?;
            // Read only the bounded local UTF-16 windows, not the whole field.
            let (before_range, after_range) = utf16_windows(total, start..end)?;
            let mut before = vec![0; before_range.len()];
            let mut after = vec![0; after_range.len()];
            CFStringGetCharacters(
                value.as_ptr(),
                CfRange {
                    location: before_range.start as isize,
                    length: before.len() as isize,
                },
                before.as_mut_ptr(),
            );
            CFStringGetCharacters(
                value.as_ptr(),
                CfRange {
                    location: after_range.start as isize,
                    length: after.len() as isize,
                },
                after.as_mut_ptr(),
            );
            let (before, after) = bounded_utf16_edges(&before, &after)?;
            let elapsed = started.elapsed();
            if elapsed >= BUDGET {
                return None;
            }
            Some(FieldContextSnapshot {
                before_cursor: Some(before),
                after_cursor: Some(after),
                has_selection: length > 0,
                surface,
                capture_us: elapsed.as_micros() as u64,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_app_identifiers_without_using_names() {
        for (id, expected) in [
            ("com.tinyspeck.slackmacgap", AppSurfaceKind::Chat),
            ("COM.MICROSOFT.OUTLOOK", AppSurfaceKind::EmailOrDoc),
            ("com.jetbrains.intellij", AppSurfaceKind::CodeOrTerminal),
            ("com.microsoft.VSCode", AppSurfaceKind::CodeOrTerminal),
            ("com.google.Chrome.canary", AppSurfaceKind::Browser),
            ("unknown.mail.app", AppSurfaceKind::General),
        ] {
            assert_eq!(classify_surface(Some(id)), expected);
        }
        assert_eq!(classify_surface(None), AppSurfaceKind::General);
    }

    #[test]
    fn utf16_windows_preserve_unicode_and_limit_chars_not_bytes() {
        let before: Vec<u16> = format!("{}हिन्दी😀", "𠀀".repeat(200))
            .encode_utf16()
            .collect();
        let after: Vec<u16> = "😀文".repeat(100).encode_utf16().collect();
        let (before, after) = bounded_utf16_edges(&before, &after).unwrap();
        assert_eq!(before.chars().count(), 160);
        assert!(before.ends_with("हिन्दी😀"));
        assert_eq!(after.chars().count(), 80);
        assert!(after.starts_with("😀文"));
    }

    #[test]
    fn utf16_windows_drop_only_outer_partial_surrogates_and_reject_split_cursor() {
        let (before, after) = bounded_utf16_edges(&[0xdc00, 65], &[66, 0xd800]).unwrap();
        assert_eq!((before.as_str(), after.as_str()), ("A", "B"));
        assert!(bounded_utf16_edges(&[65, 0xd800], &[0xdc00, 66]).is_none());
        assert!(bounded_utf16_edges(&[0xd800, 65], &[]).is_none());
        assert_eq!(
            bounded_utf16_edges(&[], &[]),
            Some((String::new(), String::new()))
        );
    }

    #[test]
    fn selection_is_excluded_using_utf16_offsets_and_bad_ranges_are_rejected() {
        let units: Vec<u16> = "left😀selected𠀀right".encode_utf16().collect();
        let (before, after) = utf16_windows(units.len(), 6..14).unwrap();
        assert_eq!(
            bounded_utf16_edges(&units[before], &units[after]),
            Some(("left😀".into(), "𠀀right".into()))
        );
        assert!(utf16_windows(units.len(), 0..units.len() + 1).is_none());
        let (before, after) = utf16_windows(units.len(), 5..5).unwrap();
        assert!(bounded_utf16_edges(&units[before], &units[after]).is_none());
    }

    #[test]
    fn password_and_unknown_controls_are_rejected_before_reading_the_value() {
        assert!(!supports_field("AXTextField", Some("AXSecureTextField")));
        assert!(!supports_field("AXSecureTextField", None));
        assert!(!supports_field("AXTextField", None));
        assert!(!supports_field("AXButton", None));
        assert!(supports_field("AXTextField", Some("AXUnknown")));
        assert!(supports_field("AXTextArea", None));
    }
}
