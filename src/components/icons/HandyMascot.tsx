/**
 * lowyap's mascot: a violet speech bubble with a low tail, two eyes and a
 * cursor for a mouth. The outline and face follow the theme (`mascot-line`).
 */
const HandyMascot = ({ className = "" }: { className?: string }) => {
  return (
    <svg
      viewBox="0 0 151 165"
      className={className}
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
    >
      <g transform="translate(1.5 14) scale(2.3)">
        {/* Offset depth layer, like a sticker's edge. */}
        <path
          d="M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z"
          transform="translate(2.2 2.2)"
          fill="#4B33C9"
        />
        <path
          d="M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z"
          fill="#7C5CFF"
          className="stroke-mascot-line"
          strokeWidth="2"
          strokeLinejoin="round"
        />
        {/* Soft highlight on the upper-left of the bubble. */}
        <path
          d="M11 22A10 10 0 0 1 21 12H28"
          stroke="#fff"
          strokeOpacity="0.4"
          strokeWidth="2.6"
          strokeLinecap="round"
        />
        <ellipse
          cx="24"
          cy="22.5"
          rx="3"
          ry="3.6"
          className="fill-mascot-line"
        />
        <ellipse
          cx="40"
          cy="22.5"
          rx="3"
          ry="3.6"
          className="fill-mascot-line"
        />
        <rect
          x="26"
          y="31"
          width="12"
          height="4.5"
          rx="2.25"
          className="fill-mascot-line"
        />
      </g>
    </svg>
  );
};

export default HandyMascot;
