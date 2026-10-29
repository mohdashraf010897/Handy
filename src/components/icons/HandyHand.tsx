/** lowyap's mark as a UI icon: a speech bubble with a low tail and a typing cursor. */
const HandyHand = ({
  width,
  height,
}: {
  width?: number | string;
  height?: number | string;
  // The sidebar passes a className for its icons; the mark keeps its own color.
  className?: string;
}) => (
  <svg
    width={width || 126}
    height={height || 135}
    viewBox="0 0 64 64"
    className="fill-text stroke-text"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path
      d="M20 6H44A15 15 0 0 1 59 21V31A15 15 0 0 1 44 46H25.5L11.6 56.4Q7.6 59.4 8 54.4L8.6 46.6A15 15 0 0 1 5 37V21A15 15 0 0 1 20 6Z"
      fill="none"
      strokeWidth="5.5"
      strokeLinejoin="round"
    />
    <path
      d="M23 29.5H41A4.25 4.25 0 0 1 41 38H23A4.25 4.25 0 0 1 23 29.5Z"
      stroke="none"
    />
  </svg>
);

export default HandyHand;
