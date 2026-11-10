// The chart palette: the one TypeScript copy of the colours in styles.css
// (`pivot` is `--color-chart-pivot` there), for everything drawn on a canvas
// or an SVG (KLineChart, Vela, Recharts), which can't read a CSS token. A
// token change in styles.css is mirrored here by hand; in development the
// check at the bottom warns at once when the two files disagree. No other
// .ts/.tsx file under v2 holds a hex.

export const PALETTE = {
  /** The white every chart draws on (and outlined pills, marker rings). */
  surface: '#ffffff',
  canvas: '#f6f8fb',
  raised: '#e3e8ee',
  inset: '#eaeef3',
  line: '#dde3ea',
  lineStrong: '#788494',
  fg: '#161b22',
  muted: '#46505d',
  subtle: '#606c7a',
  onAccent: '#ffffff',
  accent: '#1b64da',
  profit: '#087551',
  loss: '#c62828',
  live: '#bf7407',
  liveInk: '#8a4b00',
  grid: '#e9edf2',
  indicator: '#2a6fe3',
  /** The second indicator line (`--color-chart-indicator-2`): the CCI pane's
   * violet, 5.43:1 on white. It never shares a pane with the blue: each
   * oscillator sits in its own, named in its legend. */
  indicator2: '#7a4fd1',
  up: '#0f9f80',
  down: '#e0433f',
  /** Pivot levels on both chart libraries (`--color-chart-pivot`; the owner's
   * call: orange, not line-strong): 3.19:1 on white. Their names are `muted`. */
  pivot: '#d97706',
} as const;

export const FONT = '"Inter", ui-sans-serif, system-ui, sans-serif';

/** A palette colour (`#rrggbb`) at opacity `a`, for a chart fill: the same colour, never a second one. */
export function alpha(hex: string, a: number): string {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${n >> 16}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

/**
 * What the charts draw with, by role. Charts draw on white, so `canvas` and
 * `panel` here are the white surface, not the page's grey.
 */
export const C = {
  canvas: PALETTE.surface,
  panel: PALETTE.surface,
  raised: PALETTE.raised,
  inset: PALETTE.inset,
  line: PALETTE.line,
  lineStrong: PALETTE.lineStrong,
  fg: PALETTE.fg,
  muted: PALETTE.muted,
  subtle: PALETTE.subtle,
  profit: PALETTE.profit,
  loss: PALETTE.loss,
  onAccent: PALETTE.onAccent,
  grid: PALETTE.grid,
  indicator: PALETTE.indicator,
  indicator2: PALETTE.indicator2,
  up: PALETTE.up,
  down: PALETTE.down,
  accent: PALETTE.accent,
} as const;

// Development only: the tokens every chart leans on, plus the pivot, read
// back from the stylesheet once it has applied. A mismatch is a console warning, so a
// token edited in styles.css and not here shows up on the first chart.
if (import.meta.env.DEV && typeof window !== 'undefined') {
  window.setTimeout(() => {
    const css = getComputedStyle(document.documentElement);
    const expected: [string, string][] = [
      ['--color-profit', PALETTE.profit],
      ['--color-loss', PALETTE.loss],
      ['--color-chart-indicator', PALETTE.indicator],
      ['--color-chart-indicator-2', PALETTE.indicator2],
      ['--color-chart-pivot', PALETTE.pivot],
    ];
    for (const [name, hex] of expected) {
      const got = css.getPropertyValue(name).trim().toLowerCase();
      if (got && got !== hex) console.warn(`lib/palette.ts has ${hex} for ${name} but styles.css has ${got}: keep the two files equal.`);
    }
  }, 0);
}
