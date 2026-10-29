/** The tray icon (design/brand masters); uses currentColor like a macOS template image. */
type TrayIconState = "idle" | "recording" | "transcribing";

const PATHS: Record<TrayIconState, string> = {
  idle: "M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6ZM23 30H41A4 4 0 0 1 41 38H23A4 4 0 0 1 23 30Z",
  recording:
    "M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6ZM19.5 26A12.5 12.5 0 1 0 44.5 26A12.5 12.5 0 1 0 19.5 26ZM24 26A8 8 0 1 0 40 26A8 8 0 1 0 24 26Z",
  transcribing:
    "M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6ZM14.75 26A4.75 4.75 0 1 0 24.25 26A4.75 4.75 0 1 0 14.75 26ZM27.25 26A4.75 4.75 0 1 0 36.75 26A4.75 4.75 0 1 0 27.25 26ZM39.75 26A4.75 4.75 0 1 0 49.25 26A4.75 4.75 0 1 0 39.75 26Z",
};

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
    className="fill-current"
    xmlns="http://www.w3.org/2000/svg"
  >
    <path fillRule="evenodd" d={PATHS[state]} />
  </svg>
);

export default TrayIcon;
