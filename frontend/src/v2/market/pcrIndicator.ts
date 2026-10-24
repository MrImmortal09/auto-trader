// The PCR pane under the Market futures chart: put ÷ call at each bar, the
// thresholds the option signal flips on, and a dot on the latest value.
// Registered once, globally.

import { registerIndicator } from 'klinecharts';
import { C, FONT } from '../trade/chartSetup';

export interface PcrSeries {
  /** PCR by bar start (Unix ms): the latest check at that bar's close. */
  values: Map<number, number>;
  buyAbove: number;
  sellBelow: number;
}

/** Each chart's PCR, keyed by the chart's id: `extendData` can't carry a Map. */
export const PCR_SERIES = new Map<string, PcrSeries>();

/** The threshold lines and their words: one line when both are equal. */
export function thresholdLines(s: Pick<PcrSeries, 'buyAbove' | 'sellBelow'>): { value: number; text: string }[] {
  if (s.buyAbove === s.sellBelow) return [{ value: s.buyAbove, text: `${s.buyAbove.toFixed(2)} · buy above, sell below` }];
  return [
    { value: s.buyAbove, text: `${s.buyAbove.toFixed(2)} · buy above` },
    { value: s.sellBelow, text: `${s.sellBelow.toFixed(2)} · sell below` },
  ];
}

registerIndicator<{ pcr?: number }, number, { key: string }>({
  name: 'v2Pcr',
  shortName: 'PCR',
  precision: 2,
  // Bumped to make the chart recalculate when the checks change.
  calcParams: [0],
  figures: [{ key: 'pcr', title: '', type: 'line' }],
  calc: (dataList, indicator) => {
    const series = PCR_SERIES.get(indicator.extendData?.key ?? '');
    return dataList.map((d) => ({ pcr: series?.values.get(d.timestamp) }));
  },
  draw: ({ ctx, bounding, xAxis, yAxis, indicator }) => {
    const series = PCR_SERIES.get(indicator.extendData?.key ?? '');
    if (!series) return false;
    ctx.save();
    ctx.lineWidth = 1;
    ctx.strokeStyle = C.muted;
    ctx.fillStyle = C.muted;
    ctx.font = `11px ${FONT}`;
    ctx.textAlign = 'right';
    ctx.textBaseline = 'bottom';
    ctx.setLineDash([4, 4]);
    for (const line of thresholdLines(series)) {
      const y = Math.round(yAxis.convertToPixel(line.value)) + 0.5;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(bounding.width, y);
      ctx.stroke();
      ctx.fillText(line.text, bounding.width - 8, y - 3);
    }
    ctx.setLineDash([]);
    // The latest check: a dot at the line's end.
    const result = indicator.result;
    for (let i = result.length - 1; i >= 0; i--) {
      const v = result[i]?.pcr;
      if (v == null) continue;
      ctx.fillStyle = C.indicator;
      ctx.beginPath();
      ctx.arc(xAxis.convertToPixel(i), yAxis.convertToPixel(v), 3.5, 0, Math.PI * 2);
      ctx.fill();
      break;
    }
    ctx.restore();
    return false;
  },
});
