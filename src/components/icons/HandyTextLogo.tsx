/**
 * The "lowyap" wordmark: monoline lowercase letters in the theme's text color,
 * finished with the brand's violet typing cursor.
 */
const HandyTextLogo = ({
  width,
  height,
  className,
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  return (
    <svg
      width={width}
      height={height}
      className={className}
      viewBox="0 0 394 102"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label="lowyap"
    >
      <g
        className="stroke-text"
        strokeWidth="11"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M8 6V70" />
        <circle cx="53" cy="50" r="20" />
        <path d="M96 30V52A12 12 0 0 0 120 52V38M120 52A12 12 0 0 0 144 52V30" />
        <path d="M169 30V50A20 20 0 0 0 209 50V30M209 30V80A16 16 0 0 1 193 96H184" />
        <circle cx="254" cy="50" r="20" />
        <path d="M274 30V70" />
        <path d="M299 30V96" />
        <circle cx="319" cy="50" r="20" />
      </g>
      <rect x="356" y="64.5" width="32" height="11" rx="3" fill="#7C5CFF" />
    </svg>
  );
};

export default HandyTextLogo;
