// Official MJOT brand marks — MJOT is the online inference API the built-in
// bot talks to. Inline SVGs (same pattern as BrandMarks) rather than static
// image files so the marks adapt to the active theme: the ink maps to
// `--foreground` and the tile face to `--background`, which reproduces the
// official light variant (dark ink on white) on light themes and the official
// dark variant (white ink) on dark themes — including the tinted and custom
// palettes, where a hardcoded #1A1A1A/#FFFFFF pair would clash. The red
// accent is a fixed brand constant and never follows the theme.
//
// Path data is copied verbatim from the official logo SVGs; only the fills
// are re-expressed as theme classes. Don't edit the geometry here — replace
// it from the source assets if the brand changes.

/** Brand red. Fixed in both themes. */
const MJOT_ACCENT = '#E63946'

type MarkProps = {
  className?: string
  /** Accessible name. Omit when adjacent text already names MJOT — the mark
   *  is then decorative and hidden from the accessibility tree. */
  label?: string
}

function a11y(label?: string) {
  return label
    ? ({ role: 'img', 'aria-label': label } as const)
    : ({ 'aria-hidden': true } as const)
}

/** The MJOT tile mark alone (no wordmark) — for tight spots like dialog
 *  headers. Roughly square (286×341 viewBox); size it with a height class
 *  plus `w-auto`. */
export function MjotMark({ className, label }: MarkProps) {
  return (
    <svg
      viewBox="223 368 286 341"
      className={className}
      shapeRendering="geometricPrecision"
      {...a11y(label)}
    >
      <MarkPaths />
    </svg>
  )
}

/** The full MJOT lockup: tile mark + wordmark (1042×381 viewBox). The
 *  wordmark already reads "MJOT", so when this is used as a heading pass
 *  `label="MJOT"` instead of repeating the name in visible text. */
export function MjotLogo({ className, label }: MarkProps) {
  return (
    <svg
      viewBox="203 348 1042 381"
      className={className}
      shapeRendering="geometricPrecision"
      {...a11y(label)}
    >
      <MarkPaths />
      <g>
        <path
          className="fill-foreground"
          d="M579 625V452H605L654 526L703 452H729V625H703V497L654 571L605 497V625H579Z"
        />
        <path
          className="fill-foreground"
          d="M818 452H844V605L824 625H774L754 605V571H780V593L786 599H818V452Z"
        />
        <path
          className="fill-foreground"
          fillRule="evenodd"
          d="M891 452H977L999 474V603L977 625H891L869 603V474L891 452ZM895 485L902 478H966L973 485V592L966 599H902L895 592V485Z"
        />
        <rect fill={MJOT_ACCENT} x="921" y="525.5" width="26" height="26" rx="8" />
        <path className="fill-foreground" d="M1024 452H1164V478H1107V625H1081V478H1024Z" />
      </g>
    </svg>
  )
}

/** The shared tile-mark geometry (the "M" tile with its accent). */
function MarkPaths() {
  return (
    <g>
      <path
        className="fill-background"
        d="M261 388H447C457 388 465 397 465 407V654C465 665 457 673 447 673H262C251 673 242 664 242 653V408C242 397 250 388 261 388Z"
      />
      <path className="fill-background" d="M484 418C488 421 490 425 490 428V667C488 665 486 660 484 653V418Z" />
      <path
        className="fill-foreground"
        fillRule="evenodd"
        d="M266 372H447C456 372 464 375 471 382L480 391L491 400C498 407 502 416 502 426V675C502 692 489 705 471 705H279C267 705 256 701 247 692L238 684C230 676 227 667 227 657V406C227 387 247 372 266 372ZM261 388H447C457 388 465 397 465 407V654C465 665 457 673 447 673H262C251 673 242 664 242 653V408C242 397 250 388 261 388ZM484 418C488 421 490 425 490 428V667C488 665 486 660 484 653V418Z"
      />
      <path
        className="fill-foreground"
        d="M263 633V426H293L352.5 515L412 426H442V633H412V480L352.5 569L293 480V633H263Z"
      />
      <rect fill={MJOT_ACCENT} x="329" y="590" width="47" height="22" rx="5" />
    </g>
  )
}
