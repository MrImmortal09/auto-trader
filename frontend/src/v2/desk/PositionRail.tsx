import { useLayoutEffect, useRef, useState, type CSSProperties } from 'react';
import type { MarkTone, RailMark, RailSpec } from './rail';

// Figma "Position rail": where the price sits between the stop and the
// targets. States Up, Down, T1 hit, Scalp up, Waiting and None — see rail.ts.

const TICK: Record<MarkTone, string> = { loss: 'bg-loss', profit: 'bg-profit', muted: 'bg-fg-muted' };
const TEXT: Record<MarkTone, string> = { loss: 'text-loss', profit: 'text-profit', muted: 'text-fg-muted' };

/** Roughly how wide a caption is: 11 px Anek Latin, digits condensed. */
const textWidth = (s: string) => s.length * 6 + 4;

/** Where a label goes, kept inside the rail: centred on its point, or held
 * against an end. */
function labelBox(cx: number, w: number, width: number): { left: number; style: CSSProperties } {
  if (cx - w / 2 < 0) return { left: 0, style: { left: 0 } };
  if (cx + w / 2 > width) return { left: width - w, style: { right: 0 } };
  return { left: cx - w / 2, style: { left: cx, transform: 'translateX(-50%)' } };
}

export function PositionRail({ spec }: { spec: RailSpec }) {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  if (spec.empty) {
    return (
      <div className="relative h-[46px] w-full">
        <div className="absolute inset-x-0 top-[23px] border-t-2 border-dashed border-line" />
        <div className="absolute inset-x-0 top-[29px] truncate text-center type-caption text-fg-muted">{spec.empty}</div>
      </div>
    );
  }

  const values = [
    ...spec.marks.map((m) => m.at),
    ...(spec.marker ? [spec.marker.at] : []),
    ...(spec.zone ? [spec.zone.from, spec.zone.to] : []),
  ].filter(Number.isFinite);
  let lo = Math.min(...values);
  let hi = Math.max(...values);
  if (!(hi - lo > 1e-9)) {
    lo -= 1;
    hi += 1;
  }
  const pct = (v: number) => ((v - lo) / (hi - lo)) * 100;

  // Labels by priority, dropping any that would collide with one placed.
  const placed: { mark: RailMark; style: CSSProperties }[] = [];
  const taken: [number, number][] = [];
  for (const mark of spec.marks.filter((m) => m.label).sort((a, b) => a.priority - b.priority)) {
    const w = textWidth(mark.label);
    const { left, style } = labelBox((pct(mark.at) / 100) * width, w, width);
    if (width > 0 && taken.some(([a, b]) => left < b + 6 && left + w > a - 6)) continue;
    taken.push([left, left + w]);
    placed.push({ mark, style });
  }
  const marker = spec.marker;
  const markerLabel = marker ? labelBox((pct(marker.at) / 100) * width, textWidth(marker.label) + 6, width).style : undefined;

  return (
    <div ref={box} className="relative h-[46px] w-full select-none" aria-hidden>
      {marker && (
        <span className="absolute top-0 whitespace-nowrap num-small text-fg" style={markerLabel}>
          {marker.label}
        </span>
      )}
      <div className={`absolute inset-x-0 top-[23px] ${spec.dashed ? 'border-t-2 border-dashed border-line-strong' : 'h-0.5 rounded-full bg-line-strong'}`} />
      {spec.zone && (
        <div
          className="absolute top-[20px] h-2 rounded-4 border border-dashed border-profit/50 bg-profit-subtle"
          style={{ left: `${pct(spec.zone.from)}%`, width: `${pct(spec.zone.to) - pct(spec.zone.from)}%` }}
        />
      )}
      {spec.fill && (
        <div
          className={`absolute top-[22px] h-1 rounded-full ${spec.fill.tone === 'profit' ? 'bg-profit' : 'bg-loss'}`}
          style={{ left: `${Math.min(pct(spec.fill.from), pct(spec.fill.to))}%`, width: `${Math.abs(pct(spec.fill.to) - pct(spec.fill.from))}%` }}
        />
      )}
      {spec.marks
        .filter((m) => m.tick !== false)
        .map((m, i) => (
          <span key={i} className={`absolute top-[19px] h-2.5 w-0.5 -translate-x-1/2 rounded-full ${TICK[m.tone]}`} style={{ left: `${pct(m.at)}%` }} />
        ))}
      {marker && (
        <span
          className={`absolute top-[18px] size-3 -translate-x-1/2 rounded-full ${marker.hollow ? 'border-2 border-fg bg-canvas' : 'bg-fg ring-2 ring-canvas'}`}
          style={{ left: `${pct(marker.at)}%` }}
        />
      )}
      {placed.map(({ mark, style }, i) => (
        <span key={i} className={`absolute top-[31px] whitespace-nowrap type-caption tnum ${TEXT[mark.tone]}`} style={style}>
          {mark.label}
        </span>
      ))}
    </div>
  );
}
