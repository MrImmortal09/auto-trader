// KLineChart setup for the v2 Scalp charts: the theme from the design tokens,
// the order-line and marker overlays, VWAP and pivots. Registered once, globally.

import { registerIndicator, registerOverlay, utils, type DeepPartial, type OverlayFigure, type Styles } from 'klinecharts';
import { pivotLines, type PivotLevels } from './pivots';

/** The design tokens as plain values: the chart draws on a canvas. */
export const C = {
  canvas: '#1a1f29',
  panel: '#242b37',
  raised: '#2c3442',
  inset: '#141820',
  line: '#353e4d',
  lineStrong: '#4a5465',
  fg: '#e8eaee',
  muted: '#8e97a7',
  profit: '#22b47f',
  loss: '#ee705c',
  onAccent: '#1a1f29',
  grid: '#2c3442',
  indicator: '#79a7f2',
} as const;

export const FONT = '"Anek Latin", ui-sans-serif, system-ui, sans-serif';

const IN = new Intl.NumberFormat('en-IN', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

export const CHART_STYLES: DeepPartial<Styles> = {
  grid: { horizontal: { color: C.grid, style: 'solid', size: 1 }, vertical: { show: false } },
  candle: {
    type: 'candle_solid',
    bar: {
      upColor: C.profit,
      downColor: C.loss,
      noChangeColor: C.muted,
      upBorderColor: C.profit,
      downBorderColor: C.loss,
      noChangeBorderColor: C.muted,
      upWickColor: C.profit,
      downWickColor: C.loss,
      noChangeWickColor: C.muted,
    },
    priceMark: {
      high: { show: false },
      low: { show: false },
      last: {
        show: true,
        // The price tag is white whichever way the bar went; the line is quiet.
        upColor: C.fg,
        downColor: C.fg,
        noChangeColor: C.fg,
        line: { show: true, style: 'dashed', dashedValue: [3, 3], size: 1 },
        text: { show: true, color: C.onAccent, size: 11, family: FONT, weight: 600, paddingLeft: 4, paddingRight: 4, paddingTop: 3, paddingBottom: 2, borderRadius: 2 },
        // The countdown to the bar's close, under the price.
        extendTexts: [{ show: true, position: 'below_price', updateInterval: 1000, color: C.onAccent, size: 10, family: FONT, weight: 500, paddingLeft: 4, paddingRight: 4, paddingTop: 1, paddingBottom: 2, borderRadius: 2 }],
      },
    },
    tooltip: { showRule: 'none' },
  },
  indicator: {
    lines: [{ color: C.indicator, size: 1.5, style: 'solid', smooth: false }],
    lastValueMark: { show: false },
    tooltip: {
      showRule: 'always',
      title: { show: true, showName: true, showParams: false, color: C.indicator, size: 11, family: FONT },
      legend: { color: C.indicator, size: 11, family: FONT },
    },
  },
  xAxis: {
    axisLine: { show: false },
    tickLine: { show: false },
    tickText: { color: C.muted, size: 11, family: FONT },
  },
  yAxis: {
    axisLine: { show: false },
    tickLine: { show: false },
    tickText: { color: C.muted, size: 11, family: FONT },
  },
  separator: { color: C.line },
  crosshair: {
    horizontal: {
      line: { color: C.lineStrong, style: 'dashed', dashedValue: [3, 3] },
      text: { color: C.fg, backgroundColor: C.raised, borderColor: C.lineStrong, family: FONT, size: 11 },
    },
    vertical: {
      line: { color: C.lineStrong, style: 'dashed', dashedValue: [3, 3] },
      text: { color: C.fg, backgroundColor: C.raised, borderColor: C.lineStrong, family: FONT, size: 11 },
    },
  },
};

/** Indian digit grouping on the axis: 25,112.40. */
export const THOUSANDS = { sign: ',', format: (value: string | number) => IN.format(Number(value)) };

// ── Order lines ─────────────────────────────────────────────────────────── //

/** A pill on an order line: its label, or a button ("+ Stop", "×"). */
export interface LinePart {
  key: string;
  text: string;
  color: string;
  /** Filled with `color`, text dark; else outlined. */
  filled?: boolean;
  /** Drawn before the text: a grip (drag me) or a lock (read-only). */
  icon?: 'grip' | 'lock';
}

const ICON_W = 10;

/** A grip's six dots or a padlock, at `x` (left) and `y` (middle). */
function iconFigures(icon: 'grip' | 'lock', x: number, y: number, color: string): OverlayFigure[] {
  if (icon === 'grip') {
    return [-3, 0, 3].flatMap((dy) =>
      [0, 3].map((dx) => ({ type: 'circle', attrs: { x: x + 1 + dx, y: y + dy, r: 0.9 }, styles: { style: 'fill', color }, ignoreEvent: true })),
    );
  }
  return [
    { type: 'rect', attrs: { x, y: y - 1, width: 7, height: 5 }, styles: { style: 'fill', color, borderRadius: 1 }, ignoreEvent: true },
    { type: 'arc', attrs: { x: x + 3.5, y: y - 1, r: 2.5, startAngle: Math.PI, endAngle: 2 * Math.PI }, styles: { style: 'solid', color, size: 1.2 }, ignoreEvent: true },
  ];
}

export interface OrderLineData {
  color: string;
  dashed: boolean;
  /** The pills, left to right, right-aligned against the axis. Called on every
   * draw with the line's price, so a dragged line's label follows it. */
  parts: (price: number) => LinePart[];
  /** The price tag on the axis. */
  axisText: (price: number) => string;
  /** Can be dragged up and down. */
  draggable: boolean;
}

const PILL = { size: 11, paddingX: 6, paddingY: 3, gap: 4 };
const NO_DRAG: OverlayFigure['ignoreEvent'] = ['onPressedMoveStart', 'onPressedMoving', 'onPressedMoveEnd'];

registerOverlay<OrderLineData>({
  name: 'v2OrderLine',
  totalStep: 2,
  needDefaultPointFigure: false,
  needDefaultXAxisFigure: false,
  needDefaultYAxisFigure: false,
  createPointFigures: ({ coordinates, bounding, overlay }) => {
    const d = overlay.extendData;
    const y = coordinates[0]?.y;
    if (!d || y == null || !Number.isFinite(y)) return [];
    const price = overlay.points[0]?.value ?? 0;
    const figures: OverlayFigure[] = [
      {
        key: 'line',
        type: 'line',
        attrs: { coordinates: [{ x: 0, y }, { x: bounding.width, y }] },
        styles: { style: d.dashed ? 'dashed' : 'solid', dashedValue: [4, 4], color: d.color, size: 1 },
        // The hairline never drags: pressing near it pans the chart, so a
        // stop can only move from its label.
        ignoreEvent: NO_DRAG,
      },
    ];
    // Pills from the right edge leftwards.
    let right = bounding.width - 6;
    for (const part of [...d.parts(price)].reverse()) {
      const lead = part.icon ? ICON_W : 0;
      const w = utils.calcTextWidth(part.text, PILL.size, 500, FONT) + PILL.paddingX * 2 + lead;
      figures.push({
        key: part.key,
        type: 'text',
        attrs: { x: right - w, y, text: part.text, align: 'left', baseline: 'middle' },
        styles: {
          style: 'stroke_fill',
          color: part.filled ? C.onAccent : part.color,
          backgroundColor: part.filled ? part.color : C.canvas,
          borderColor: part.color,
          borderSize: 1,
          borderRadius: 4,
          size: PILL.size,
          family: FONT,
          weight: 500,
          paddingLeft: PILL.paddingX + lead,
          paddingRight: PILL.paddingX,
          paddingTop: PILL.paddingY,
          paddingBottom: PILL.paddingY,
        },
        // Only the label drags; buttons are clicked.
        ignoreEvent: d.draggable && part.key === 'label' ? false : NO_DRAG,
      });
      if (part.icon) figures.push(...iconFigures(part.icon, right - w + PILL.paddingX, y, part.filled ? C.onAccent : part.color));
      right -= w + PILL.gap;
    }
    return figures;
  },
  createYAxisFigures: ({ coordinates, overlay }) => {
    const d = overlay.extendData;
    const y = coordinates[0]?.y;
    if (!d || y == null || !Number.isFinite(y)) return [];
    return {
      type: 'text',
      attrs: { x: 0, y, text: d.axisText(overlay.points[0]?.value ?? 0), align: 'left', baseline: 'middle' },
      styles: {
        style: 'fill',
        color: C.onAccent,
        backgroundColor: d.color,
        size: 11,
        family: FONT,
        weight: 600,
        paddingLeft: 4,
        paddingRight: 4,
        paddingTop: 3,
        paddingBottom: 2,
        borderRadius: 2,
      },
      ignoreEvent: true,
    };
  },
});

// ── Markers ─────────────────────────────────────────────────────────────── //

export interface MarkerData {
  /** B (buy), E (exit), T (target), S (stop) — or a Telegram call. */
  kind: 'B' | 'E' | 'T' | 'S' | 'call';
  /** Above the bar (exits, calls) or below it (buys). */
  above: boolean;
}

const MARKER_COLOR: Record<MarkerData['kind'], string> = { B: C.profit, E: C.fg, T: C.profit, S: C.loss, call: C.indicator };

registerOverlay<MarkerData>({
  name: 'v2Marker',
  totalStep: 2,
  needDefaultPointFigure: false,
  needDefaultXAxisFigure: false,
  needDefaultYAxisFigure: false,
  createPointFigures: ({ coordinates, overlay }) => {
    const d = overlay.extendData;
    const c = coordinates[0];
    if (!d || !c || !Number.isFinite(c.x) || !Number.isFinite(c.y)) return [];
    const r = 8;
    const y = c.y + (d.above ? -(r + 6) : r + 6);
    const figures: OverlayFigure[] = [
      { type: 'circle', attrs: { x: c.x, y, r }, styles: { style: 'stroke_fill', color: MARKER_COLOR[d.kind], borderColor: C.canvas, borderSize: 2 }, ignoreEvent: true },
    ];
    if (d.kind === 'call') {
      // A paper plane, as the Telegram chip has.
      figures.push({
        type: 'polygon',
        attrs: { coordinates: [{ x: c.x - 4, y: y + 0.5 }, { x: c.x + 4, y: y - 4 }, { x: c.x + 1, y: y + 4 }, { x: c.x - 0.5, y: y + 1 }] },
        styles: { style: 'fill', color: C.onAccent },
        ignoreEvent: true,
      });
    } else {
      figures.push({
        type: 'text',
        attrs: { x: c.x, y, text: d.kind, align: 'center', baseline: 'middle' },
        styles: { style: 'fill', color: C.onAccent, backgroundColor: 'transparent', size: 10, family: FONT, weight: 700, paddingLeft: 0, paddingRight: 0, paddingTop: 0, paddingBottom: 0 },
        ignoreEvent: true,
      });
    }
    return figures;
  },
});

// ── VWAP ────────────────────────────────────────────────────────────────── //

/** Each chart's VWAP by bar start (Unix ms), keyed by the chart's id. The
 * indicator reads it here: KLineChart can't carry a Map in `extendData`. */
export const VWAP_SERIES = new Map<string, Map<number, number>>();

registerIndicator<{ vwap?: number }, number, { key: string }>({
  name: 'v2Vwap',
  shortName: 'VWAP',
  series: 'price',
  precision: 2,
  // Bumped to make the chart recalculate when new bars change the series.
  calcParams: [0],
  figures: [{ key: 'vwap', title: '', type: 'line' }],
  calc: (dataList, indicator) => {
    const series = VWAP_SERIES.get(indicator.extendData?.key ?? '');
    return dataList.map((d) => ({ vwap: series?.get(d.timestamp) }));
  },
});

// ── Pivot points ────────────────────────────────────────────────────────── //

/** Each chart's pivots by IST day number, keyed by the chart's id. */
export const PIVOT_SERIES = new Map<string, Map<number, PivotLevels>>();

/** TradingView's pivot orange, for every level and its name. */
const PIVOT_ORANGE = '#fb8c00';
const IST_OFFSET = 330 * 60_000;
const dayNumber = (t: number) => Math.floor((t + IST_OFFSET) / 86_400_000);

// Standard pivots, drawn straight onto the candle pane the way TradingView
// draws them: solid orange levels across each day, named at the day's start.
// No figures, so the price axis never stretches to take in a far R5 or S5.
registerIndicator<Record<string, never>, number, { key: string }>({
  name: 'v2Pivots',
  shortName: 'Pivots',
  series: 'price',
  // Bumped to redraw when the levels change.
  calcParams: [0],
  figures: [],
  calc: (dataList) => dataList.map(() => ({})),
  // No legend row: the levels carry their own names.
  createTooltipDataSource: () => ({ name: '', calcParamsText: '', legends: [], features: [] }),
  draw: ({ ctx, chart, bounding, xAxis, yAxis, indicator }) => {
    const byDay = PIVOT_SERIES.get(indicator.extendData?.key ?? '');
    if (!byDay || byDay.size === 0) return true;
    const data = chart.getDataList();
    const { from, to } = chart.getVisibleRange();
    const start = Math.max(0, from);
    const end = Math.min(data.length - 1, to);
    if (end < start) return true;
    const half = Math.max(1, (xAxis.convertToPixel(start + 1) - xAxis.convertToPixel(start)) / 2);
    // The latest day's levels run on to the right edge, and are always named.
    const latest = dayNumber(data[data.length - 1].timestamp);
    ctx.save();
    ctx.lineWidth = 1;
    ctx.strokeStyle = PIVOT_ORANGE;
    ctx.fillStyle = PIVOT_ORANGE;
    ctx.font = `10px ${FONT}`;
    ctx.textBaseline = 'bottom';
    ctx.textAlign = 'left';
    let i = start;
    while (i <= end) {
      const day = dayNumber(data[i].timestamp);
      let j = i;
      while (j + 1 <= end && dayNumber(data[j + 1].timestamp) === day) j++;
      const levels = byDay.get(day);
      if (levels) {
        const current = day === latest;
        const x0 = Math.max(xAxis.convertToPixel(i) - half, 0);
        const x1 = current ? bounding.width : Math.min(xAxis.convertToPixel(j) + half, bounding.width);
        // A past day is named only where there's room for the names.
        const named = current || x1 - x0 > 80;
        for (const line of pivotLines(levels)) {
          const y = Math.round(yAxis.convertToPixel(line.value)) + 0.5;
          if (y < 0 || y > bounding.height) continue;
          ctx.beginPath();
          ctx.moveTo(x0, y);
          ctx.lineTo(x1, y);
          ctx.stroke();
          if (named) ctx.fillText(`${line.name} (${IN.format(line.value)})`, x0 + 4, y - 2);
        }
      }
      i = j + 1;
    }
    ctx.restore();
    return true;
  },
});
