import type { ReactNode } from 'react';
import { AlertTriangle, CheckCircle2, MinusCircle, XCircle } from 'lucide-react';
import { SERIES, STATUS_COLOR, type Level } from '../../lib/pulse';

export const CARD = 'bg-surface-container-lowest border border-outline-variant rounded-xl shadow-sm min-w-0';
export const CARD_HEADER =
  'flex items-center gap-2 px-4 py-3 border-b border-outline-variant bg-surface-container-low text-sm font-bold text-on-surface';

const STATUS_ICON = {
  good: CheckCircle2,
  warning: AlertTriangle,
  critical: XCircle,
  neutral: MinusCircle,
} as const;

/** Status = icon in the reserved status color + a text label. Never color alone. */
export function StatusBadge({ level, label }: { level: Level; label: string }) {
  const Icon = STATUS_ICON[level];
  return (
    <span className="inline-flex items-center gap-1 shrink-0 text-[11px] font-semibold text-on-surface-variant">
      <Icon size={14} style={{ color: STATUS_COLOR[level] }} aria-hidden="true" />
      {label}
    </span>
  );
}

/** A single ratio against a limit. The fill carries severity; the track is a
 * lighter step of the accent ramp. */
export function Meter({ pct, level, label }: { pct: number; level: Level; label: string }) {
  const clamped = Math.max(0, Math.min(100, pct));
  const fill = level === 'warning' || level === 'critical' ? STATUS_COLOR[level] : SERIES.primary;
  return (
    <div
      className="h-1.5 w-full rounded-full bg-primary-fixed overflow-hidden"
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(clamped)}
    >
      <div className="h-full rounded-full transition-[width] duration-500" style={{ width: `${clamped}%`, background: fill }} />
    </div>
  );
}

/** Inox-style KPI card: category · title · value · detail · meter · footer. */
export function PulseCard({
  category,
  title,
  icon,
  value,
  detail,
  meter,
  footer,
  level,
  statusLabel,
}: {
  category: string;
  title: string;
  icon: ReactNode;
  value: ReactNode;
  detail?: ReactNode;
  meter?: { pct: number; level: Level; label: string };
  footer?: ReactNode;
  level: Level;
  statusLabel: string;
}) {
  return (
    <div className={`${CARD} p-4 flex flex-col gap-3`}>
      <div className="flex items-start justify-between gap-2">
        <div className="flex items-center gap-2.5 min-w-0">
          <div className="p-2 rounded-lg bg-surface-container text-primary shrink-0">{icon}</div>
          <div className="min-w-0">
            <div className="text-[10px] font-bold uppercase tracking-wider text-on-surface-variant">{category}</div>
            <div className="text-sm font-semibold text-on-surface truncate">{title}</div>
          </div>
        </div>
        <StatusBadge level={level} label={statusLabel} />
      </div>
      <div className="text-2xl font-bold text-on-surface leading-none">{value}</div>
      {detail && <div className="text-xs text-on-surface-variant">{detail}</div>}
      {meter && <Meter {...meter} />}
      {footer && (
        <div className="text-[11px] text-on-surface-variant border-t border-outline-variant pt-2 mt-auto">{footer}</div>
      )}
    </div>
  );
}

/** Section card with a header row. */
export function Section({
  icon,
  title,
  aside,
  children,
}: {
  icon: ReactNode;
  title: string;
  aside?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className={`${CARD} overflow-hidden`}>
      <div className={`${CARD_HEADER} justify-between flex-wrap gap-y-1`}>
        <span className="flex items-center gap-2">
          {icon}
          {title}
        </span>
        {aside}
      </div>
      <div className="p-4">{children}</div>
    </div>
  );
}

/** Small labelled number; used in dense grids inside sections. */
export function Figure({ label, value, hint }: { label: string; value: ReactNode; hint?: ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-[11px] text-on-surface-variant">{label}</div>
      <div className="text-base font-semibold text-on-surface">{value}</div>
      {hint && <div className="text-[11px] text-on-surface-variant truncate">{hint}</div>}
    </div>
  );
}

/** Horizontal bars for one series (one hue), largest first. Values are always
 * printed beside the bar, so nothing depends on reading bar length. */
export function BarList({ rows, empty }: { rows: { label: string; value: number }[]; empty: string }) {
  const shown = rows.filter((r) => r.value > 0).sort((a, b) => b.value - a.value);
  if (!shown.length) return <div className="text-xs text-on-surface-variant py-2">{empty}</div>;
  const max = shown[0].value;
  return (
    <ul className="space-y-2">
      {shown.map((r) => (
        <li key={r.label} className="grid grid-cols-[minmax(0,10rem)_1fr_auto] items-center gap-3 text-xs">
          <span className="text-on-surface truncate" title={r.label}>{r.label}</span>
          <span className="h-2 bg-surface-container rounded-sm overflow-hidden">
            <span
              className="block h-full rounded-r-[4px]"
              style={{ width: `${Math.max(2, (r.value / max) * 100)}%`, background: SERIES.primary }}
            />
          </span>
          <span className="tabular-nums font-semibold text-on-surface text-right min-w-8">{r.value.toLocaleString('en-IN')}</span>
        </li>
      ))}
    </ul>
  );
}

/** Connection / subsystem row: name, state badge, one line of detail. */
export function StatusRow({ name, level, state, detail }: { name: string; level: Level; state: string; detail?: string }) {
  return (
    <div className="flex items-center justify-between gap-3 py-2 border-b border-outline-variant last:border-0">
      <div className="min-w-0">
        <div className="text-sm font-medium text-on-surface">{name}</div>
        {detail && <div className="text-[11px] text-on-surface-variant truncate">{detail}</div>}
      </div>
      <StatusBadge level={level} label={state} />
    </div>
  );
}
