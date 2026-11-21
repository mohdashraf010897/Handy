//! Automatic macOS system microphone input volume upgrade during dictation,
//! with exact restore on recording completion, shutdown, or crash recovery.
//!
//! Many laptop and headset microphones end up parked at low OS input slider
//! positions (e.g. `20%–35%` after video-call auto-adjustments), dropping speech
//! below `-45 dBFS`. Raising the OS input gain to [`DEFAULT_TARGET_PERCENT`]
//! while recording and restoring the user's prior setting afterward improves
//! hardware SNR without permanently altering system preferences.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

/// Default target macOS input volume percentage applied while recording.
pub const DEFAULT_TARGET_PERCENT: u8 = 85;

const MIN_TARGET_PERCENT: u8 = 60;
const MAX_TARGET_PERCENT: u8 = 100;

/// Tolerance (in percentage points) when comparing current OS volume against
/// `applied_percent` to account for scalar float <-> integer rounding while
/// still detecting deliberate manual slider drags by the user.
const MANUAL_CHANGE_TOLERANCE_PERCENT: u8 = 5;

const RESTORE_FILE_NAME: &str = "mic_gain_restore.json";

const _: () = assert!(
    DEFAULT_TARGET_PERCENT >= MIN_TARGET_PERCENT && DEFAULT_TARGET_PERCENT <= MAX_TARGET_PERCENT
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ActiveBoost {
    initial_percent: u8,
    applied_percent: u8,
}

pub(crate) trait VolumeBackend {
    fn get_volume(&self) -> Option<u8>;
    fn set_volume(&self, percent: u8) -> bool;
}

struct SystemVolumeBackend;

impl VolumeBackend for SystemVolumeBackend {
    fn get_volume(&self) -> Option<u8> {
        current_input_volume_percent()
    }

    fn set_volume(&self, percent: u8) -> bool {
        set_system_input_volume(percent)
    }
}

/// Reads the current system default microphone input volume (`0..=100`).
///
/// Returns `None` on non-macOS platforms or on digital/fixed-gain input devices
/// (such as HDMI or Bluetooth HFP/A2DP devices without OS input gain control).
pub fn current_input_volume_percent() -> Option<u8> {
    #[cfg(target_os = "macos")]
    {
        macos::get_macos_input_volume()
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn set_system_input_volume(percent: u8) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::set_macos_input_volume(percent)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = percent;
        false
    }
}

/// Tracks an active temporary microphone input volume boost so it can be
/// restored accurately without clobbering manual user adjustments.
pub struct MicGainSession {
    active: Mutex<Option<ActiveBoost>>,
}

impl Default for MicGainSession {
    fn default() -> Self {
        Self::new()
    }
}

impl MicGainSession {
    pub const fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }

    /// Raises the system input volume to `target_percent` (clamped to `60..=100`)
    /// when `enabled` is true and the current input volume is lower, returning
    /// both the pre-boost volume (when changed) and the confirmed effective OS
    /// input volume percentage (`0..=100`, or `None` on fixed-gain devices).
    pub fn apply_and_effective_volume(
        &self,
        enabled: bool,
        target_percent: u8,
        app_data_dir: Option<&Path>,
    ) -> (Option<u8>, Option<u8>) {
        self.apply_and_effective_with_backend(
            &SystemVolumeBackend,
            enabled,
            target_percent,
            app_data_dir,
        )
    }

    /// Raises the system input volume to `target_percent` (clamped to `60..=100`)
    /// Restores the user's pre-recording input volume if a boost is active and
    /// the user did not manually move the OS input volume slider mid-recording.
    pub fn restore(&self, app_data_dir: Option<&Path>) -> bool {
        self.restore_with_backend(&SystemVolumeBackend, app_data_dir)
    }

    #[cfg(test)]
    pub(crate) fn apply_with_backend<B: VolumeBackend>(
        &self,
        backend: &B,
        enabled: bool,
        target_percent: u8,
        app_data_dir: Option<&Path>,
    ) -> Option<u8> {
        self.apply_and_effective_with_backend(backend, enabled, target_percent, app_data_dir)
            .0
    }

    pub(crate) fn apply_and_effective_with_backend<B: VolumeBackend>(
        &self,
        backend: &B,
        enabled: bool,
        target_percent: u8,
        app_data_dir: Option<&Path>,
    ) -> (Option<u8>, Option<u8>) {
        let target_percent = target_percent.clamp(MIN_TARGET_PERCENT, MAX_TARGET_PERCENT);
        if !enabled {
            return (None, backend.get_volume());
        }

        let mut guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = *guard {
            debug!("Mic gain boost already active; skipping duplicate apply");
            return (None, Some(existing.applied_percent));
        }

        let Some(current) = backend.get_volume() else {
            return (None, None);
        };
        if current >= target_percent {
            debug!(
                "System mic input volume ({current}%) already >= target ({target_percent}%); leaving untouched"
            );
            return (None, Some(current));
        }

        let boost = ActiveBoost {
            initial_percent: current,
            applied_percent: target_percent,
        };

        // Persist recovery metadata before mutating OS state so a crash or SIGKILL
        // mid-session can still restore the user's original volume on next launch.
        if let Some(dir) = app_data_dir {
            if let Err(err) = write_restore_file(dir, &boost) {
                warn!("Failed to persist {RESTORE_FILE_NAME} before mic boost: {err}");
            }
        }

        if backend.set_volume(target_percent) {
            *guard = Some(boost);
            info!(
                "Boosted system mic input volume {}% -> {}%",
                current, target_percent
            );
            (Some(current), Some(target_percent))
        } else {
            warn!(
                "Failed to boost system mic input volume {}% -> {}%",
                current, target_percent
            );
            if let Some(dir) = app_data_dir {
                remove_restore_file(dir);
            }
            (None, Some(current))
        }
    }

    pub(crate) fn restore_with_backend<B: VolumeBackend>(
        &self,
        backend: &B,
        app_data_dir: Option<&Path>,
    ) -> bool {
        let active = {
            let mut guard = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            guard.take()
        };

        let Some(active) = active else {
            if let Some(dir) = app_data_dir {
                remove_restore_file(dir);
            }
            return false;
        };

        if let Some(now) = backend.get_volume() {
            if now.abs_diff(active.applied_percent) > MANUAL_CHANGE_TOLERANCE_PERCENT {
                debug!(
                    "System mic input volume changed manually ({}% -> {}%); keeping user volume instead of restoring {}%",
                    active.applied_percent, now, active.initial_percent
                );
                if let Some(dir) = app_data_dir {
                    remove_restore_file(dir);
                }
                return false;
            }
        }

        if backend.set_volume(active.initial_percent) {
            if let Some(dir) = app_data_dir {
                remove_restore_file(dir);
            }
            info!(
                "Restored system mic input volume to {}%",
                active.initial_percent
            );
            true
        } else {
            warn!(
                "Failed to restore system mic input volume to {}%; keeping recovery marker",
                active.initial_percent
            );
            false
        }
    }
}

/// Startup crash-recovery hook: inspects `<app_data_dir>/mic_gain_restore.json`
/// left behind by an unclean exit, restores `initial_percent` if the OS slider
/// is still near `applied_percent`, and deletes the recovery file.
pub fn recover_orphaned_boost(app_data_dir: &Path) -> bool {
    recover_orphaned_boost_with_backend(&SystemVolumeBackend, app_data_dir)
}

pub(crate) fn recover_orphaned_boost_with_backend<B: VolumeBackend>(
    backend: &B,
    app_data_dir: &Path,
) -> bool {
    let path = restore_file_path(app_data_dir);
    if !path.exists() {
        return false;
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            warn!("Failed to read orphaned {RESTORE_FILE_NAME}: {err}");
            remove_restore_file(app_data_dir);
            return false;
        }
    };

    let orphaned: ActiveBoost = match serde_json::from_slice::<ActiveBoost>(&bytes) {
        Ok(parsed) if parsed.initial_percent <= 100 && parsed.applied_percent <= 100 => parsed,
        Ok(invalid) => {
            warn!("Ignoring out-of-range orphaned {RESTORE_FILE_NAME} ({invalid:?})");
            remove_restore_file(app_data_dir);
            return false;
        }
        Err(err) => {
            warn!("Failed to parse orphaned {RESTORE_FILE_NAME}: {err}");
            remove_restore_file(app_data_dir);
            return false;
        }
    };

    if let Some(now) = backend.get_volume() {
        if now.abs_diff(orphaned.applied_percent) > MANUAL_CHANGE_TOLERANCE_PERCENT {
            debug!(
                "Orphaned mic boost found, but current input volume ({now}%) differs from applied ({}%); keeping user setting",
                orphaned.applied_percent
            );
            remove_restore_file(app_data_dir);
            return false;
        }
    }

    let restored = backend.set_volume(orphaned.initial_percent);

    if restored {
        remove_restore_file(app_data_dir);
        info!(
            "Recovered orphaned mic gain boost: restored system mic input volume to {}%",
            orphaned.initial_percent
        );
        true
    } else {
        warn!(
            "Failed to recover orphaned system mic input volume to {}%; retaining recovery marker",
            orphaned.initial_percent
        );
        false
    }
}

fn restore_file_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(RESTORE_FILE_NAME)
}

fn write_restore_file(app_data_dir: &Path, boost: &ActiveBoost) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(app_data_dir)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(app_data_dir)?;
    }

    let path = restore_file_path(app_data_dir);
    let mut tmp_os = path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);

    let payload = serde_json::to_vec(boost)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

    let write_res = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(&tmp_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }

        file.write_all(&payload)?;
        file.sync_all()
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, &path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    Ok(())
}

fn remove_restore_file(app_data_dir: &Path) {
    let path = restore_file_path(app_data_dir);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!("Failed to remove {RESTORE_FILE_NAME}: {err}"),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_osascript_volume(stdout: &str) -> Option<u8> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("missing value") {
        return None;
    }
    let parsed = trimmed.parse::<i32>().ok()?;
    u8::try_from(parsed).ok().filter(|&v| v <= 100)
}

#[cfg(target_os = "macos")]
#[allow(non_snake_case, non_upper_case_globals)]
mod macos {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::process::Command;

    use super::parse_osascript_volume;

    type AudioObjectID = u32;
    type OSStatus = i32;
    type AudioObjectPropertySelector = u32;
    type AudioObjectPropertyScope = u32;
    type AudioObjectPropertyElement = u32;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    struct AudioObjectPropertyAddress {
        mSelector: AudioObjectPropertySelector,
        mScope: AudioObjectPropertyScope,
        mElement: AudioObjectPropertyElement,
    }

    const kAudioObjectUnknown: AudioObjectID = 0;
    const kAudioObjectSystemObject: AudioObjectID = 1;
    const kAudioHardwareNoError: OSStatus = 0;

    const kAudioHardwarePropertyDefaultInputDevice: AudioObjectPropertySelector =
        u32::from_be_bytes(*b"dIn ");
    const kAudioObjectPropertyScopeGlobal: AudioObjectPropertyScope = u32::from_be_bytes(*b"glob");
    const kAudioDevicePropertyScopeInput: AudioObjectPropertyScope = u32::from_be_bytes(*b"inpt");
    const kAudioObjectPropertyElementMain: AudioObjectPropertyElement = 0;
    const kAudioDevicePropertyVolumeScalar: AudioObjectPropertySelector =
        u32::from_be_bytes(*b"volm");

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectHasProperty(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
        ) -> u8;

        fn AudioObjectIsPropertySettable(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            outIsSettable: *mut u8,
        ) -> OSStatus;

        fn AudioObjectGetPropertyData(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            inQualifierDataSize: u32,
            inQualifierData: *const c_void,
            ioDataSize: *mut u32,
            outData: *mut c_void,
        ) -> OSStatus;

        fn AudioObjectSetPropertyData(
            inObjectID: AudioObjectID,
            inAddress: *const AudioObjectPropertyAddress,
            inQualifierDataSize: u32,
            inQualifierData: *const c_void,
            inDataSize: u32,
            inData: *const c_void,
        ) -> OSStatus;
    }

    const fn volume_address(element: AudioObjectPropertyElement) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyVolumeScalar,
            mScope: kAudioDevicePropertyScopeInput,
            mElement: element,
        }
    }

    fn default_input_device() -> Option<AudioObjectID> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultInputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };

        // SAFETY: `address` points to a valid `AudioObjectPropertyAddress` on the stack.
        if unsafe { AudioObjectHasProperty(kAudioObjectSystemObject, &address) } == 0 {
            return None;
        }

        let mut device_id: AudioObjectID = kAudioObjectUnknown;
        let mut data_size = size_of::<AudioObjectID>() as u32;
        // SAFETY: `device_id` and `data_size` are initialized and match `size_of::<u32>()`.
        let status = unsafe {
            AudioObjectGetPropertyData(
                kAudioObjectSystemObject,
                &address,
                0,
                std::ptr::null(),
                &mut data_size,
                (&mut device_id as *mut AudioObjectID).cast::<c_void>(),
            )
        };

        if status == kAudioHardwareNoError && device_id != kAudioObjectUnknown {
            Some(device_id)
        } else {
            None
        }
    }

    fn is_volume_settable(device_id: AudioObjectID, element: AudioObjectPropertyElement) -> bool {
        let address = volume_address(element);
        // SAFETY: `address` points to a valid stack `AudioObjectPropertyAddress`.
        if unsafe { AudioObjectHasProperty(device_id, &address) } == 0 {
            return false;
        }
        let mut settable: u8 = 0;
        // SAFETY: `settable` is a valid out-pointer to a single `Boolean` (`u8`).
        let status = unsafe { AudioObjectIsPropertySettable(device_id, &address, &mut settable) };
        status == kAudioHardwareNoError && settable != 0
    }

    /// Selects CoreAudio element `0` (main/master) if present and settable,
    /// falling back to element `1` (channel 1) before deferring to AppleScript.
    fn select_settable_volume_element(
        device_id: AudioObjectID,
    ) -> Option<AudioObjectPropertyElement> {
        [kAudioObjectPropertyElementMain, 1]
            .into_iter()
            .find(|&element| is_volume_settable(device_id, element))
    }

    fn get_coreaudio_input_volume() -> Option<u8> {
        let device_id = default_input_device()?;
        let element = select_settable_volume_element(device_id)?;
        let address = volume_address(element);

        let mut scalar: f32 = 0.0;
        let mut data_size = size_of::<f32>() as u32;
        // SAFETY: `scalar` and `data_size` match CoreAudio's `Float32` scalar property ABI.
        let status = unsafe {
            AudioObjectGetPropertyData(
                device_id,
                &address,
                0,
                std::ptr::null(),
                &mut data_size,
                (&mut scalar as *mut f32).cast::<c_void>(),
            )
        };
        if status != kAudioHardwareNoError || !scalar.is_finite() {
            return None;
        }
        Some((scalar.clamp(0.0, 1.0) * 100.0).round() as u8)
    }

    fn set_coreaudio_element_scalar(
        device_id: AudioObjectID,
        element: AudioObjectPropertyElement,
        scalar: f32,
    ) -> bool {
        let address = volume_address(element);
        // SAFETY: `scalar` is an initialized `f32` of `size_of::<f32>()` bytes.
        let status = unsafe {
            AudioObjectSetPropertyData(
                device_id,
                &address,
                0,
                std::ptr::null(),
                size_of::<f32>() as u32,
                (&scalar as *const f32).cast::<c_void>(),
            )
        };
        status == kAudioHardwareNoError
    }

    fn set_coreaudio_input_volume(percent: u8) -> bool {
        let Some(device_id) = default_input_device() else {
            return false;
        };
        let Some(element) = select_settable_volume_element(device_id) else {
            return false;
        };
        let scalar = f32::from(percent.min(100)) / 100.0;
        let ok = set_coreaudio_element_scalar(device_id, element, scalar);
        // Keep stereo input channels balanced on devices without master element 0.
        if ok && element == 1 && is_volume_settable(device_id, 2) {
            let _ = set_coreaudio_element_scalar(device_id, 2, scalar);
        }
        ok
    }

    fn get_osascript_input_volume() -> Option<u8> {
        let output = Command::new("osascript")
            .args(["-e", "input volume of (get volume settings)"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        parse_osascript_volume(&String::from_utf8_lossy(&output.stdout))
    }

    fn set_osascript_input_volume(percent: u8) -> bool {
        let clamped = percent.min(100);
        let script = format!("set volume input volume {clamped}");
        Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    pub(super) fn get_macos_input_volume() -> Option<u8> {
        get_coreaudio_input_volume().or_else(get_osascript_input_volume)
    }

    pub(super) fn set_macos_input_volume(percent: u8) -> bool {
        if set_coreaudio_input_volume(percent) {
            return true;
        }
        set_osascript_input_volume(percent)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::tempdir;

    use super::*;

    struct MockVolumeBackend {
        volume: Cell<Option<u8>>,
        allow_set: Cell<bool>,
    }

    impl MockVolumeBackend {
        fn new(initial: Option<u8>) -> Self {
            Self {
                volume: Cell::new(initial),
                allow_set: Cell::new(true),
            }
        }

        fn current(&self) -> Option<u8> {
            self.volume.get()
        }

        fn simulate_user_slider_drag(&self, new_percent: u8) {
            self.volume.set(Some(new_percent));
        }
    }

    impl VolumeBackend for MockVolumeBackend {
        fn get_volume(&self) -> Option<u8> {
            self.volume.get()
        }

        fn set_volume(&self, percent: u8) -> bool {
            if !self.allow_set.get() {
                return false;
            }
            self.volume.set(Some(percent));
            true
        }
    }

    fn apply_and_restore_with_backend<B: VolumeBackend>(
        session: &MicGainSession,
        backend: &B,
        enabled: bool,
        target_percent: u8,
        app_data_dir: Option<&Path>,
    ) -> (Option<u8>, Option<u8>, bool) {
        let applied_from =
            session.apply_with_backend(backend, enabled, target_percent, app_data_dir);
        let during_session = backend.get_volume();
        let restored = session.restore_with_backend(backend, app_data_dir);
        (applied_from, during_session, restored)
    }

    #[test]
    fn low_volume_upgrades_to_85_and_restores_to_27_cleaning_up_json() {
        let dir = tempdir().expect("tempdir");
        let backend = MockVolumeBackend::new(Some(27));
        let session = MicGainSession::new();

        let initial =
            session.apply_with_backend(&backend, true, DEFAULT_TARGET_PERCENT, Some(dir.path()));
        assert_eq!(initial, Some(27));
        assert_eq!(backend.current(), Some(85));
        assert!(restore_file_path(dir.path()).exists());

        let restored = session.restore_with_backend(&backend, Some(dir.path()));
        assert!(restored);
        assert_eq!(backend.current(), Some(27));
        assert!(!restore_file_path(dir.path()).exists());
    }

    #[test]
    fn already_high_volume_is_left_untouched() {
        let dir = tempdir().expect("tempdir");
        let backend = MockVolumeBackend::new(Some(90));
        let session = MicGainSession::new();

        let (applied_from, during, restored) = apply_and_restore_with_backend(
            &session,
            &backend,
            true,
            DEFAULT_TARGET_PERCENT,
            Some(dir.path()),
        );
        assert_eq!(applied_from, None);
        assert_eq!(during, Some(90));
        assert!(!restored);
        assert_eq!(backend.current(), Some(90));
        assert!(!restore_file_path(dir.path()).exists());
    }

    #[test]
    fn duplicate_apply_does_not_overwrite_initial_27_with_85() {
        let dir = tempdir().expect("tempdir");
        let backend = MockVolumeBackend::new(Some(27));
        let session = MicGainSession::new();

        assert_eq!(
            session.apply_with_backend(&backend, true, DEFAULT_TARGET_PERCENT, Some(dir.path())),
            Some(27)
        );
        assert_eq!(backend.current(), Some(85));

        // Duplicate start while already active must never overwrite `initial_percent = 27`.
        assert_eq!(
            session.apply_with_backend(&backend, true, 95, Some(dir.path())),
            None
        );
        assert_eq!(backend.current(), Some(85));

        assert!(session.restore_with_backend(&backend, Some(dir.path())));
        assert_eq!(backend.current(), Some(27));
    }

    #[test]
    fn manual_user_slider_move_during_session_is_respected_on_restore() {
        let dir = tempdir().expect("tempdir");
        let backend = MockVolumeBackend::new(Some(27));
        let session = MicGainSession::new();

        assert_eq!(
            session.apply_with_backend(&backend, true, DEFAULT_TARGET_PERCENT, Some(dir.path())),
            Some(27)
        );
        assert_eq!(backend.current(), Some(85));

        // User drags macOS input slider from 85% -> 50% while recording.
        backend.simulate_user_slider_drag(50);

        let restored = session.restore_with_backend(&backend, Some(dir.path()));
        assert!(!restored);
        assert_eq!(backend.current(), Some(50));
        assert!(!restore_file_path(dir.path()).exists());
    }

    #[test]
    fn crash_recovery_restores_27_verifies_0600_mode_and_deletes_restore_json() {
        let dir = tempdir().expect("tempdir");
        let backend = MockVolumeBackend::new(Some(27));

        {
            let crashed_session = MicGainSession::new();
            assert_eq!(
                crashed_session.apply_with_backend(
                    &backend,
                    true,
                    DEFAULT_TARGET_PERCENT,
                    Some(dir.path()),
                ),
                Some(27)
            );
            assert_eq!(backend.current(), Some(85));

            let restore_path = restore_file_path(dir.path());
            assert!(restore_path.exists());

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&restore_path)
                    .expect("restore metadata")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600);
            }
            // Drop `crashed_session` without calling `restore` to simulate force-quit/crash.
        }

        assert!(recover_orphaned_boost_with_backend(&backend, dir.path()));
        assert_eq!(backend.current(), Some(27));
        assert!(!restore_file_path(dir.path()).exists());

        // Subsequent launch is a clean no-op.
        assert!(!recover_orphaned_boost_with_backend(&backend, dir.path()));
    }

    #[test]
    fn osascript_missing_value_and_fixed_gain_devices_return_none_cleanly() {
        assert_eq!(parse_osascript_volume("missing value\n"), None);
        assert_eq!(parse_osascript_volume("Missing Value"), None);
        assert_eq!(parse_osascript_volume(""), None);
        assert_eq!(parse_osascript_volume("-5"), None);
        assert_eq!(parse_osascript_volume("101"), None);
        assert_eq!(parse_osascript_volume("27\n"), Some(27));
        assert_eq!(parse_osascript_volume("0"), Some(0));
        assert_eq!(parse_osascript_volume("100"), Some(100));

        let digital_device = MockVolumeBackend::new(None);
        let session = MicGainSession::new();
        assert_eq!(
            session.apply_with_backend(&digital_device, true, DEFAULT_TARGET_PERCENT, None),
            None
        );
        assert!(!session.restore_with_backend(&digital_device, None));

        // If `set_volume` fails on a read-only device, pre-written restore JSON is cleaned up.
        let dir = tempdir().expect("tempdir");
        let read_only = MockVolumeBackend::new(Some(27));
        read_only.allow_set.set(false);
        assert_eq!(
            session.apply_with_backend(&read_only, true, DEFAULT_TARGET_PERCENT, Some(dir.path()),),
            None
        );
        assert!(!restore_file_path(dir.path()).exists());
    }
}
