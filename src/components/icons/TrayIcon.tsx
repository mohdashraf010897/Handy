/**
 * lowyap menu-bar tray icon: "Low-Frequency Terrace" (V2, locked permanent baseline).
 * Uses `currentColor` like a macOS AppKit template mask (`as_template = true` in idle/transcribing),
 * with Signal Vermilion live LED + underscore accent during active recording.
 */
type TrayIconState = "idle" | "recording" | "transcribing";

const TrayIcon = ({
  state,
  size,
}: {
  state: TrayIconState;
  size: number | string;
}) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 64 64"
    fill="none"
    xmlns="http://www.w3.org/2000/svg"
  >
    {state === "idle" && (
      <>
        <rect x="8" y="26" width="7.4" height="22" rx="3.7" fill="currentColor" />
        <rect x="20" y="13" width="7.4" height="35" rx="3.7" fill="currentColor" />
        <rect x="32" y="20" width="7.4" height="28" rx="3.7" fill="currentColor" />
        <rect x="43.5" y="46" width="15.5" height="7.2" rx="3.6" fill="currentColor" />
      </>
    )}
    {state === "recording" && (
      <>
        <rect x="8" y="26" width="7.4" height="22" rx="3.7" fill="currentColor" />
        <rect x="20" y="13" width="7.4" height="35" rx="3.7" fill="currentColor" />
        <rect x="32" y="20" width="7.4" height="28" rx="3.7" fill="currentColor" />
        <circle cx="51.25" cy="23" r="6.5" fill="var(--color-logo-primary)" />
        <rect
          x="43.5"
          y="46"
          width="15.5"
          height="7.2"
          rx="3.6"
          fill="var(--color-logo-primary)"
        />
      </>
    )}
    {state === "transcribing" && (
      <>
        <circle cx="13" cy="31" r="4.6" fill="currentColor" />
        <circle cx="26" cy="31" r="4.6" fill="currentColor" />
        <circle cx="39" cy="31" r="4.6" fill="currentColor" />
        <rect x="43.5" y="46" width="15.5" height="7.2" rx="3.6" fill="currentColor" />
      </>
    )}
  </svg>
);

export default TrayIcon;
