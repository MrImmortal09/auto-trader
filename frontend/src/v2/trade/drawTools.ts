// The drawing tools KLineChart doesn't ship, as its own kind of overlay, so
// they draw, drag, snap, lock and hide like its built-in ones: a rectangle,
// a path of any number of points, a text label and the measuring ruler.
// Registered once, globally.

import { registerOverlay, type OverlayFigure } from 'klinecharts';
import { MINUS } from '../lib/format';
import { alpha } from '../lib/palette';
import { C, FONT } from './chartSetup';

const IN = new Intl.NumberFormat('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** The most points a path takes; double-click or Enter ends it sooner. */
export const PATH_MAX_POINTS = 30;

// A box between two corners.
registerOverlay({
  name: 'v2Rect',
  totalStep: 3,
  needDefaultPointFigure: true,
  needDefaultXAxisFigure: true,
  needDefaultYAxisFigure: true,
  createPointFigures: ({ coordinates }) => {
    if (coordinates.length < 2) return [];
    const [a, b] = coordinates;
    return [
      {
        type: 'polygon',
        attrs: { coordinates: [a, { x: b.x, y: a.y }, b, { x: a.x, y: b.y }] },
        styles: { style: 'stroke_fill' },
      },
    ];
  },
});

// Straight lines through every point tapped.
registerOverlay({
  name: 'v2Path',
  totalStep: PATH_MAX_POINTS + 1,
  needDefaultPointFigure: true,
  needDefaultXAxisFigure: true,
  needDefaultYAxisFigure: true,
  createPointFigures: ({ coordinates }) => (coordinates.length < 2 ? [] : [{ type: 'line', attrs: { coordinates } }]),
});

// A label at a point, in the accent; "Text" until it is written.
registerOverlay<string>({
  name: 'v2Text',
  totalStep: 2,
  needDefaultPointFigure: true,
  needDefaultXAxisFigure: false,
  needDefaultYAxisFigure: false,
  createPointFigures: ({ coordinates, overlay }) => {
    const at = coordinates[0];
    if (!at) return [];
    const text = typeof overlay.extendData === 'string' && overlay.extendData !== '' ? overlay.extendData : 'Text';
    return [
      {
        type: 'text',
        attrs: { x: at.x + 6, y: at.y, text, align: 'left', baseline: 'middle' },
        styles: { color: C.accent, size: 13, family: FONT, weight: 500, backgroundColor: alpha(C.accent, 0.08), borderColor: alpha(C.accent, 0.35), borderSize: 1, borderRadius: 4, paddingLeft: 5, paddingRight: 5, paddingTop: 3, paddingBottom: 3 },
      },
    ];
  },
});

/** "1h 15m", "2d 3h", "45m". */
function span(ms: number): string {
  const m = Math.round(Math.abs(ms) / 60_000);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

const signed = (n: number, text: string) => `${n > 0 ? '+' : n < 0 ? MINUS : ''}${text}`;

// The ruler: from the first tap to the second, the move in points and
// percent, the bars and the time between, shaded in the candles' up or down
// colour. Measured, never kept.
registerOverlay({
  name: 'v2Ruler',
  totalStep: 3,
  needDefaultPointFigure: false,
  needDefaultXAxisFigure: true,
  needDefaultYAxisFigure: true,
  createPointFigures: ({ coordinates, overlay }) => {
    if (coordinates.length < 2) return [];
    const [a, b] = coordinates;
    const [p, q] = overlay.points;
    const from = p?.value;
    const to = q?.value;
    if (from == null || to == null || !Number.isFinite(from) || !Number.isFinite(to)) return [];
    const move = to - from;
    const tone = move >= 0 ? C.up : C.down;
    const bars = p.dataIndex != null && q.dataIndex != null ? Math.abs(q.dataIndex - p.dataIndex) : null;
    const time = p.timestamp != null && q.timestamp != null ? span(q.timestamp - p.timestamp) : null;
    const lines = [
      `${signed(move, IN.format(Math.abs(move)))} (${signed(move, `${Math.abs(from ? (move / from) * 100 : 0).toFixed(2)}%`)})`,
      [bars != null ? `${bars} bar${bars === 1 ? '' : 's'}` : null, time].filter(Boolean).join(', '),
    ].filter(Boolean);
    // The label sits past the end the move went to, centred across the box.
    const x = (a.x + b.x) / 2;
    const below = b.y >= a.y;
    const figures: OverlayFigure[] = [
      {
        type: 'polygon',
        attrs: { coordinates: [a, { x: b.x, y: a.y }, b, { x: a.x, y: b.y }] },
        styles: { style: 'stroke_fill', color: alpha(tone, 0.12), borderColor: alpha(tone, 0.6), borderSize: 1 },
        ignoreEvent: true,
      },
      {
        type: 'line',
        attrs: { coordinates: [{ x, y: a.y }, { x, y: b.y }] },
        styles: { color: tone, size: 1, style: 'dashed', dashedValue: [3, 3] },
        ignoreEvent: true,
      },
    ];
    lines.forEach((text, i) => {
      const n = below ? i : lines.length - 1 - i;
      figures.push({
        type: 'text',
        attrs: { x, y: below ? b.y + 6 + n * 18 : b.y - 6 - n * 18, text, align: 'center', baseline: below ? 'top' : 'bottom' },
        styles: { color: C.onAccent, size: 11, family: FONT, weight: 600, backgroundColor: tone, borderColor: tone, borderRadius: 3, paddingLeft: 5, paddingRight: 5, paddingTop: 2, paddingBottom: 2 },
        ignoreEvent: true,
      });
    });
    return figures;
  },
});
