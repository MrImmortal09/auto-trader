// KLineChart's own indicators, picked per chart from the Indicators menu and
// drawn by the library with its default settings. VWAP, pivots, RSI and CCI
// are the Layers menu's (see `chartSetup.ts`). Vela has its own menu.

import type { Chart, DeepPartial, IndicatorStyle } from 'klinecharts';
import { alpha } from '../lib/palette';
import { C } from './chartSetup';

export interface Study {
  /** KLineChart's name for it. */
  name: string;
  title: string;
  /** Its settings, as KLineChart has them. */
  note: string;
  /** Drawn over the price, else in a pane of its own under it. */
  over: boolean;
  /** Built from volume: an index has none, so only an option's chart can draw it. */
  volume?: boolean;
}

export const STUDIES: Study[] = [
  { name: 'MA', title: 'MA · Moving average', note: '5, 10, 30 and 60 bars', over: true },
  { name: 'EMA', title: 'EMA · Exponential moving average', note: '6, 12 and 20 bars', over: true },
  { name: 'SMA', title: 'SMA · Smoothed moving average', note: '12 bars, weight 2', over: true },
  { name: 'BOLL', title: 'BOLL · Bollinger bands', note: '20 bars, 2 standard deviations', over: true },
  { name: 'SAR', title: 'SAR · Parabolic SAR', note: 'Step 0.02, up to 0.2', over: true },
  { name: 'BBI', title: 'BBI · Bull and bear index', note: 'Mean of the 3, 6, 12 and 24-bar averages', over: true },
  { name: 'VOL', title: 'VOL · Volume', note: 'With 5, 10 and 20-bar averages', over: false, volume: true },
  { name: 'MACD', title: 'MACD', note: '12, 26, 9', over: false },
  { name: 'KDJ', title: 'KDJ · Stochastic', note: '9, 3, 3', over: false },
  { name: 'DMI', title: 'DMI · Directional movement', note: '14, 6', over: false },
  { name: 'WR', title: 'WR · Williams %R', note: '6, 10 and 14 bars', over: false },
  { name: 'MTM', title: 'MTM · Momentum', note: '12 bars, 6-bar average', over: false },
  { name: 'ROC', title: 'ROC · Rate of change', note: '12 bars, 6-bar average', over: false },
  { name: 'TRIX', title: 'TRIX · Triple-smoothed average', note: '12 bars, 9-bar average', over: false },
  { name: 'BIAS', title: 'BIAS · Distance from the average', note: '6, 12 and 24 bars', over: false },
  { name: 'AO', title: 'AO · Awesome oscillator', note: '5 and 34 bars', over: false },
  { name: 'PSY', title: 'PSY · Psychological line', note: '12 bars, 6-bar average', over: false },
  { name: 'BRAR', title: 'BRAR · Sentiment', note: '26 bars', over: false },
  { name: 'CR', title: 'CR · Energy', note: '26 bars', over: false },
  { name: 'DMA', title: 'DMA · Difference of averages', note: '10, 50, 10', over: false },
  { name: 'OBV', title: 'OBV · On-balance volume', note: '30-bar average', over: false, volume: true },
  { name: 'PVT', title: 'PVT · Price and volume trend', note: 'From the first bar', over: false, volume: true },
  { name: 'VR', title: 'VR · Volume ratio', note: '26 bars, 6-bar average', over: false, volume: true },
  { name: 'EMV', title: 'EMV · Ease of movement', note: '14 bars, 9-bar average', over: false, volume: true },
];

const BY_NAME = new Map(STUDIES.map((s) => [s.name, s]));

/** The saved picks that are still studies, each once, in the menu's order. */
export function knownStudies(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  return STUDIES.filter((s) => v.includes(s.name)).map((s) => s.name);
}

/** What `sym` can draw of `picked`: an index has no volume. */
export function drawableStudies(picked: string[], index: boolean): string[] {
  return picked.filter((n) => !(index && BY_NAME.get(n)?.volume));
}

// The lines of one indicator, in order: the indicator blue and violet, then
// pink, brown and slate, a hue apart from the price's teal and red (the Line
// style is the teal) and the pivots' orange.
const LINES = [C.indicator, C.indicator2, '#db2777', '#92400e', '#475569'];
const STYLES: DeepPartial<IndicatorStyle> = {
  lines: LINES.map((color) => ({ color, size: 1.5, style: 'solid', smooth: false, dashedValue: [2, 2] })),
  bars: [{ style: 'fill', upColor: alpha(C.up, 0.6), downColor: alpha(C.down, 0.6), noChangeColor: C.muted, borderStyle: 'solid', borderSize: 1, borderDashedValue: [2, 2] }],
  circles: [{ style: 'stroke', upColor: C.up, downColor: C.down, noChangeColor: C.muted, borderStyle: 'solid', borderSize: 1, borderDashedValue: [2, 2] }],
};

const paneOf = (name: string) => `study_${name.toLowerCase()}_pane`;
const PRICE_PANE = 'candle_pane';
/** The panes under the price take at most this much of the chart. */
const UNDER_SHARE = 0.45;
const PANE_MIN = 40;

/**
 * Sizes the panes under the price on `chart` (RSI, CCI and the studies'). A
 * new pane gets an equal share of `UNDER_SHARE` of a chart `height` tall, up
 * to 100 px (72 on a phone); one already sized keeps its height, dragged or
 * not, until together they take more than that share, when all are shared
 * out again. The share wins over `PANE_MIN`, so the price always keeps the
 * rest. `sized` is the chart's own record of the panes it has sized.
 */
export function fitPanes(chart: Chart, height: number, compact: boolean, sized: Set<string>) {
  const panes = [...new Set(chart.getIndicators().map((i) => i.paneId))].filter((id) => id !== PRICE_PANE);
  for (const id of [...sized]) if (!panes.includes(id)) sized.delete(id);
  if (panes.length === 0 || height <= 0) return;
  const cap = height * UNDER_SHARE;
  const each = Math.max(1, Math.min(compact ? 72 : 100, Math.floor(cap / panes.length)));
  const size = (id: string) => {
    chart.setPaneOptions({ id, height: each, minHeight: Math.min(PANE_MIN, each) });
    sized.add(id);
  };
  panes.filter((id) => !sized.has(id)).forEach(size);
  const taken = panes.reduce((sum, id) => sum + (chart.getSize(id)?.height ?? 0), 0);
  if (taken > cap + 1) panes.forEach(size);
}

/** Shows exactly `names` of the studies on `chart`: adds what's missing,
 * takes away what's no longer picked. Those under the price get a pane each. */
export function showStudies(chart: Chart, names: string[], compact: boolean) {
  const want = new Set(names);
  for (const s of STUDIES) {
    const paneId = s.over ? PRICE_PANE : paneOf(s.name);
    const shown = chart.getIndicators({ name: s.name, paneId }).length > 0;
    if (shown && !want.has(s.name)) chart.removeIndicator({ name: s.name, paneId });
    if (!shown && want.has(s.name)) {
      chart.createIndicator({ name: s.name, paneId, styles: STYLES }, s.over);
      if (!s.over) chart.setPaneOptions({ id: paneId, height: compact ? 72 : 100, minHeight: 56 });
    }
  }
}
