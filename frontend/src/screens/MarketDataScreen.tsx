import { useEffect, useState } from 'react';
import { AlertTriangle, RotateCcw, SlidersHorizontal } from 'lucide-react';
import { apiFetch } from '../lib/api';
import { fmt } from '../lib/format';

// ---------------------------------------------------------------------------
// Market data (Upstox option chain → OI / PCR / VWAP trend). Read-only analytics;
// nothing on this screen affects order placement.
// ---------------------------------------------------------------------------

type IndexId = 'NIFTY' | 'BANKNIFTY' | 'SENSEX';
type Basis = 'chg' | 'total';
/** +1 = BUY, -1 = SELL, 0 = no signal. */
type Score = -1 | 0 | 1;

const INDICES: { id: IndexId; label: string }[] = [
  { id: 'NIFTY', label: 'NIFTY 50' },
  { id: 'BANKNIFTY', label: 'BANK NIFTY' },
  { id: 'SENSEX', label: 'SENSEX' },
];
const WINDOWS = [
  { value: 5, label: 'ATM ±5' },
  { value: 10, label: 'ATM ±10' },
  { value: 20, label: 'ATM ±20' },
  { value: 0, label: 'All strikes' },
];
/** Must match `TREND_INTERVALS` in backend routes/market.rs. */
const INTERVALS = [5, 15, 30, 60];

// ---------------------------------------------------------------------------
// Signal settings (per-browser, localStorage)
// ---------------------------------------------------------------------------

interface SignalSettings {
  /** Minutes per trend row; the index cards score the last *closed* interval. */
  interval: number;
  basis: Basis;
  window: number;
  /** Option Signal = BUY when PCR is above this. */
  pcrBuyAbove: number;
  /** Option Signal = SELL when PCR is below this. */
  pcrSellBelow: number;
  /** VWAP Signal is neutral while the future is within this % of VWAP. */
  vwapBandPct: number;
}

const DEFAULT_SETTINGS: SignalSettings = {
  interval: 15,
  basis: 'chg',
  window: 10,
  pcrBuyAbove: 1,
  pcrSellBelow: 1,
  vwapBandPct: 0,
};
const SETTINGS_KEY = 'market_signal_settings_v1';

function finite(v: unknown, fallback: number) {
  const n = Number(v);
  return Number.isFinite(n) ? n : fallback;
}

function sanitize(raw: Partial<SignalSettings>): SignalSettings {
  const interval = INTERVALS.includes(raw.interval as number) ? (raw.interval as number) : DEFAULT_SETTINGS.interval;
  const basis: Basis = raw.basis === 'total' ? 'total' : 'chg';
  const window = WINDOWS.some((w) => w.value === raw.window) ? (raw.window as number) : DEFAULT_SETTINGS.window;
  const pcrBuyAbove = Math.max(0, finite(raw.pcrBuyAbove, DEFAULT_SETTINGS.pcrBuyAbove));
  const pcrSellBelow = Math.min(pcrBuyAbove, Math.max(0, finite(raw.pcrSellBelow, DEFAULT_SETTINGS.pcrSellBelow)));
  const vwapBandPct = Math.min(5, Math.max(0, finite(raw.vwapBandPct, DEFAULT_SETTINGS.vwapBandPct)));
  return { interval, basis, window, pcrBuyAbove, pcrSellBelow, vwapBandPct };
}

function loadSettings(): SignalSettings {
  try {
    const saved = localStorage.getItem(SETTINGS_KEY);
    if (saved) return sanitize(JSON.parse(saved));
  } catch {
    // unavailable storage / bad JSON → defaults
  }
  return DEFAULT_SETTINGS;
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

interface OiSums {
  call_oi: number;
  put_oi: number;
  call_oi_chg: number;
  put_oi_chg: number;
}
interface WindowAgg extends OiSums {
  strike_window: number;
  min_strike: number;
  max_strike: number;
}
interface LiveIndex {
  underlying: IndexId;
  expiry: string | null;
  spot: number | null;
  atm_strike: number | null;
  future_symbol: string | null;
  fut_ltp: number | null;
  fut_vwap: number | null;
  updated_at: string | null;
  last_error: string | null;
  windows: WindowAgg[];
}
interface LiveResp {
  configured: boolean;
  indices: LiveIndex[];
}
interface TrendRow extends OiSums {
  time: string;
  live: boolean;
  ts: string;
  expiry: string;
  spot: number;
  fut_ltp: number | null;
  fut_vwap: number | null;
}
interface TrendResp {
  date: string | null;
  rows: TrendRow[];
}
interface StrikeRow {
  strike: number;
  call_oi: number;
  call_prev_oi: number;
  call_ltp: number;
  call_iv: number;
  put_oi: number;
  put_prev_oi: number;
  put_ltp: number;
  put_iv: number;
}
interface ChainResp {
  atm_strike: number | null;
  strikes: StrikeRow[];
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

const intFmt = new Intl.NumberFormat('en-IN', { maximumFractionDigits: 0 });
const n0 = (v: number) => intFmt.format(Math.round(v));

function sides(basis: Basis, r: OiSums): [number, number] {
  return basis === 'chg' ? [r.call_oi_chg, r.put_oi_chg] : [r.call_oi, r.put_oi];
}

// PCR is only meaningful when both sides are positive (change-in-OI can go negative).
function pcr(call: number, put: number): number | null {
  return call > 0 && put > 0 ? put / call : null;
}

/** +1 BUY / -1 SELL from PCR thresholds; falls back to the sign of Put − Call
 * when PCR is undefined (a negative change-in-OI side). */
function optionScore(call: number, put: number, s: SignalSettings): Score {
  const ratio = pcr(call, put);
  if (ratio != null) {
    if (ratio > s.pcrBuyAbove) return 1;
    if (ratio < s.pcrSellBelow) return -1;
    return 0;
  }
  return put > call ? 1 : put < call ? -1 : 0;
}

/** +1 BUY when the future trades above VWAP (+ band), -1 below (− band). */
function vwapScore(price: number | null, vwap: number | null, s: SignalSettings): Score {
  if (price == null || vwap == null) return 0;
  const band = (vwap * s.vwapBandPct) / 100;
  if (price > vwap + band) return 1;
  if (price < vwap - band) return -1;
  return 0;
}

interface Scored {
  call: number;
  put: number;
  diff: number;
  ratio: number | null;
  option: Score;
  vwap: Score;
  /** option + vwap, from -2 to +2. */
  overall: number;
}

function scoreRow(r: OiSums & { fut_ltp: number | null; fut_vwap: number | null }, s: SignalSettings): Scored {
  const [call, put] = sides(s.basis, r);
  const option = optionScore(call, put, s);
  const vwap = vwapScore(r.fut_ltp, r.fut_vwap, s);
  return { call, put, diff: put - call, ratio: pcr(call, put), option, vwap, overall: option + vwap };
}

const tone = (v: number) => (v > 0 ? 'text-secondary' : v < 0 ? 'text-error' : 'text-on-surface-variant');

function SignalText({ score }: { score: Score }) {
  if (score === 0) return <span className="text-on-surface-variant">—</span>;
  return <span className={`font-bold ${score > 0 ? 'text-secondary' : 'text-error'}`}>{score > 0 ? 'BUY' : 'SELL'}</span>;
}

function OverallBadge({ score, size = 'sm' }: { score: number; size?: 'sm' | 'lg' }) {
  const label = score > 0 ? 'BUY' : score < 0 ? 'SELL' : 'NEUTRAL';
  const strong = Math.abs(score) >= 2;
  const color =
    score > 0
      ? strong ? 'bg-secondary text-on-secondary' : 'bg-secondary/15 text-secondary'
      : score < 0
        ? strong ? 'bg-error text-on-error' : 'bg-error/15 text-error'
        : 'bg-surface-container text-on-surface-variant';
  const pad = size === 'lg' ? 'px-2.5 py-1 text-sm' : 'px-2 py-0.5 text-[11px]';
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-md font-bold tabular-nums whitespace-nowrap ${pad} ${color}`}>
      {score > 0 ? `+${score}` : score} {label}
    </span>
  );
}

// ---------------------------------------------------------------------------
// UI helpers
// ---------------------------------------------------------------------------

function Segmented<T extends string | number>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="inline-flex flex-wrap rounded-lg border border-outline-variant overflow-hidden text-xs font-semibold">
      {options.map((o) => (
        <button
          key={String(o.value)}
          onClick={() => onChange(o.value)}
          className={`px-3 py-1.5 transition-colors ${
            o.value === value
              ? 'bg-primary-container text-on-primary'
              : 'bg-surface-container-lowest text-on-surface-variant hover:bg-surface-container'
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint: string; children: React.ReactNode }) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-on-surface-variant">{label}</span>
      {children}
      <span className="text-[11px] text-on-surface-variant">{hint}</span>
    </label>
  );
}

const inputClass =
  'w-28 rounded-lg border border-outline-variant bg-surface-container-lowest px-2.5 py-1.5 text-xs text-on-surface tabular-nums focus:outline-none focus:border-primary';

function usePolling<T>(serverBase: string, path: string, everyMs: number): T | null {
  const [data, setData] = useState<T | null>(null);
  useEffect(() => {
    let cancelled = false;
    const load = () =>
      apiFetch(serverBase, path)
        .then((r) => (r.ok ? r.json() : null))
        .then((d) => {
          if (!cancelled && d) setData(d as T);
        })
        .catch(() => {});
    load();
    const timer = setInterval(load, everyMs);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [serverBase, path, everyMs]);
  return data;
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

export function MarketDataScreen({ serverBase }: { serverBase: string }) {
  const [selected, setSelected] = useState<IndexId>('NIFTY');
  const [draft, setDraft] = useState<Partial<SignalSettings>>(loadSettings);
  const s = sanitize(draft);

  function update(patch: Partial<SignalSettings>) {
    setDraft((prev) => {
      const next = { ...prev, ...patch };
      try {
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(sanitize(next)));
      } catch {
        // storage unavailable — settings just won't persist
      }
      return next;
    });
  }

  function resetSettings() {
    setDraft(DEFAULT_SETTINGS);
    try {
      localStorage.removeItem(SETTINGS_KEY);
    } catch {
      // ignore
    }
  }

  const trendPath = (id: IndexId) => `/api/market/intraday-trend?underlying=${id}&interval=${s.interval}&window=${s.window}`;
  const live = usePolling<LiveResp>(serverBase, '/api/market/live', 20_000);
  const trends: Record<IndexId, TrendResp | null> = {
    NIFTY: usePolling<TrendResp>(serverBase, trendPath('NIFTY'), 30_000),
    BANKNIFTY: usePolling<TrendResp>(serverBase, trendPath('BANKNIFTY'), 30_000),
    SENSEX: usePolling<TrendResp>(serverBase, trendPath('SENSEX'), 30_000),
  };
  const chain = usePolling<ChainResp>(serverBase, `/api/market/option-chain?underlying=${selected}`, 30_000);

  const trend = trends[selected];
  const liveSel = live?.indices.find((i) => i.underlying === selected);
  const aggSel = liveSel?.windows.find((w) => w.strike_window === s.window);
  const inWindow = (strike: number) => aggSel != null && strike >= aggSel.min_strike && strike <= aggSel.max_strike;

  // Option-chain table: ATM ± 20 strikes (or whole chain when "All strikes").
  const chainRows = (() => {
    const rows = chain?.strikes ?? [];
    if (s.window === 0 || chain?.atm_strike == null) return rows;
    const atm = rows.findIndex((r) => r.strike === chain.atm_strike);
    if (atm < 0) return rows;
    return rows.slice(Math.max(0, atm - 20), atm + 21);
  })();

  return (
    <div className="space-y-6">
      {live && !live.configured && (
        <div className="rounded-xl border border-amber-500/40 bg-amber-500/10 p-4 text-sm text-amber-300 flex gap-3">
          <AlertTriangle size={18} className="shrink-0 mt-0.5" />
          <div>
            Upstox market data is not configured. Generate an <b>Analytics Token</b> in the Upstox Developer Apps
            page (Analytics tab), set <code className="font-mono">UPSTOX_ANALYTICS_TOKEN</code> in the backend
            <code className="font-mono"> .env</code>, and restart the server.
          </div>
        </div>
      )}

      {/* Index summary cards — scored on the last closed interval */}
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        {INDICES.map(({ id, label }) => {
          const li = live?.indices.find((i) => i.underlying === id);
          const rows = trends[id]?.rows ?? [];
          const closed = rows.find((r) => !r.live) ?? rows[0];
          const sc = closed ? scoreRow(closed, s) : null;
          const liveAgg = li?.windows.find((w) => w.strike_window === s.window);
          const liveScore = liveAgg && li ? scoreRow({ ...liveAgg, fut_ltp: li.fut_ltp, fut_vwap: li.fut_vwap }, s) : null;
          const active = id === selected;
          return (
            <button
              key={id}
              onClick={() => setSelected(id)}
              className={`text-left bg-surface-container-lowest rounded-xl border p-4 shadow-sm transition-colors ${
                active ? 'border-primary ring-1 ring-primary' : 'border-outline-variant hover:border-outline'
              }`}
            >
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase tracking-wider text-on-surface-variant">{label}</span>
                <span className="text-[11px] text-on-surface-variant">{li?.expiry ? `Exp ${li.expiry}` : ''}</span>
              </div>
              <div className="flex items-end justify-between mt-1 gap-2">
                <span className="text-xl font-bold tabular-nums text-on-surface">{li?.spot != null ? fmt(li.spot) : '—'}</span>
                {sc ? <OverallBadge score={sc.overall} size="lg" /> : <span className="text-xs text-on-surface-variant">No score yet</span>}
              </div>
              <div className="text-[11px] text-on-surface-variant mt-1">
                {closed ? `Overall score at ${closed.time}${closed.live ? ' (first check still pending)' : ` · ${s.interval}m check`}` : ' '}
              </div>

              <div className="grid grid-cols-4 gap-2 mt-3 text-xs">
                <div>
                  <div className="text-on-surface-variant">{s.basis === 'chg' ? 'PCR ΔOI' : 'PCR OI'}</div>
                  <div className={`font-bold tabular-nums ${sc?.ratio == null ? '' : sc.ratio >= 1 ? 'text-secondary' : 'text-error'}`}>
                    {sc?.ratio != null ? sc.ratio.toFixed(2) : '—'}
                  </div>
                </div>
                <div>
                  <div className="text-on-surface-variant">Option</div>
                  {sc ? <SignalText score={sc.option} /> : '—'}
                </div>
                <div>
                  <div className="text-on-surface-variant">VWAP sig</div>
                  {sc ? <SignalText score={sc.vwap} /> : '—'}
                </div>
                <div>
                  <div className="text-on-surface-variant">Live</div>
                  {liveScore ? <span className={`font-bold tabular-nums ${tone(liveScore.overall)}`}>{liveScore.overall > 0 ? `+${liveScore.overall}` : liveScore.overall}</span> : '—'}
                </div>
              </div>

              <div className="grid grid-cols-2 gap-2 mt-3 text-xs border-t border-outline-variant pt-3">
                <div>
                  <div className="text-on-surface-variant">Overall VWAP</div>
                  <div className="font-bold tabular-nums text-on-surface">{li?.fut_vwap != null ? fmt(li.fut_vwap) : '—'}</div>
                </div>
                <div>
                  <div className="text-on-surface-variant">Future</div>
                  <div className={`font-bold tabular-nums ${li?.fut_ltp != null && li?.fut_vwap != null ? tone(li.fut_ltp - li.fut_vwap) : 'text-on-surface'}`}>
                    {li?.fut_ltp != null ? fmt(li.fut_ltp) : '—'}
                  </div>
                </div>
              </div>

              <div className="mt-3 text-[11px] text-on-surface-variant truncate">
                {li?.last_error ? (
                  <span className="text-error">{li.last_error}</span>
                ) : li?.updated_at ? (
                  `Updated ${li.updated_at.slice(11)} IST${li.future_symbol ? ` · ${li.future_symbol}` : ''}`
                ) : (
                  'Waiting for first poll (market hours only)'
                )}
              </div>
            </button>
          );
        })}
      </div>

      {/* Signal settings */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm p-4 space-y-4">
        <div className="flex items-center justify-between">
          <h3 className="flex items-center gap-2 font-bold text-on-surface text-sm">
            <SlidersHorizontal size={16} className="text-primary" /> Signal Settings
          </h3>
          <button
            onClick={resetSettings}
            className="flex items-center gap-1.5 text-xs font-semibold text-on-surface-variant hover:text-on-surface"
          >
            <RotateCcw size={13} /> Reset to defaults
          </button>
        </div>
        <div className="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-3 gap-5">
          <Field
            label="Interval"
            hint="Checked 1 min after each boundary, first check 09:16 (15m → 09:16, 09:31, 09:46…). Default 15."
          >
            <Segmented
              options={INTERVALS.map((m) => ({ value: m, label: `${m} mins` }))}
              value={s.interval}
              onChange={(v) => update({ interval: v })}
            />
          </Field>
          <Field label="OI basis" hint="Change in OI since yesterday's close, or total open interest.">
            <Segmented
              options={[
                { value: 'chg' as Basis, label: 'Change in OI' },
                { value: 'total' as Basis, label: 'Total OI' },
              ]}
              value={s.basis}
              onChange={(v) => update({ basis: v })}
            />
          </Field>
          <Field label="Strike range" hint="Strikes each side of ATM counted in Call / Put.">
            <Segmented options={WINDOWS} value={s.window} onChange={(v) => update({ window: v })} />
          </Field>
          <Field label="PCR buy above" hint={`Option Signal = BUY (+1) when PCR > ${s.pcrBuyAbove}.`}>
            <input
              type="number"
              min={0}
              step={0.05}
              value={draft.pcrBuyAbove ?? ''}
              onChange={(e) => update({ pcrBuyAbove: e.target.value === '' ? undefined : Number(e.target.value) })}
              className={inputClass}
            />
          </Field>
          <Field label="PCR sell below" hint={`Option Signal = SELL (−1) when PCR < ${s.pcrSellBelow}. Between the two = 0.`}>
            <input
              type="number"
              min={0}
              step={0.05}
              value={draft.pcrSellBelow ?? ''}
              onChange={(e) => update({ pcrSellBelow: e.target.value === '' ? undefined : Number(e.target.value) })}
              className={inputClass}
            />
          </Field>
          <Field label="VWAP neutral band (%)" hint={`VWAP Signal = 0 while the future is within ±${s.vwapBandPct}% of VWAP.`}>
            <input
              type="number"
              min={0}
              max={5}
              step={0.01}
              value={draft.vwapBandPct ?? ''}
              onChange={(e) => update({ vwapBandPct: e.target.value === '' ? undefined : Number(e.target.value) })}
              className={inputClass}
            />
          </Field>
        </div>
        <p className="text-[11px] text-on-surface-variant border-t border-outline-variant pt-3">
          Overall score = Option Signal (+1 BUY / −1 SELL / 0) + VWAP Signal (+1 / −1 / 0), from −2 to +2.
          Above 0 is BUY, below 0 is SELL, and 0 is NEUTRAL. For display only; it never places or blocks a trade.
        </p>
      </div>

      {/* Intraday trend */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm overflow-hidden">
        <div className="p-4 border-b border-outline-variant bg-surface-container-low">
          <h3 className="font-bold text-on-surface text-sm">
            Intraday Trend — {INDICES.find((i) => i.id === selected)?.label} · {s.interval} mins
          </h3>
          <p className="text-[11px] text-on-surface-variant mt-0.5">
            {s.basis === 'chg' ? 'Change in OI' : 'Total OI'}
            {aggSel && s.window !== 0 ? ` · strikes ${aggSel.min_strike}–${aggSel.max_strike}` : ' · all strikes'}
            {trend?.date ? ` · ${trend.date}` : ''}
            {liveSel?.future_symbol ? ` · VWAP: ${liveSel.future_symbol}` : ''}
          </p>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full text-xs tabular-nums">
            <thead className="text-on-surface-variant">
              <tr className="border-b border-outline-variant">
                {['Time', 'Call', 'Put', 'Diff', s.basis === 'chg' ? 'PCR ΔOI' : 'PCR OI', 'Option Signal', 'Spot', 'Future', 'VWAP', 'VWAP Signal', 'Overall'].map((h) => (
                  <th key={h} className="px-3 py-2.5 font-semibold text-center whitespace-nowrap">{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {(trend?.rows ?? []).map((r) => {
                const sc = scoreRow(r, s);
                return (
                  <tr key={r.ts} className="border-b border-outline-variant/50 hover:bg-surface-container text-center">
                    <td className="px-3 py-2 font-semibold text-on-surface whitespace-nowrap">
                      {r.time}
                      {r.live && <span className="ml-1 text-[10px] text-primary">live</span>}
                    </td>
                    <td className="px-3 py-2 text-on-surface">{n0(sc.call)}</td>
                    <td className="px-3 py-2 text-on-surface">{n0(sc.put)}</td>
                    <td className={`px-3 py-2 font-semibold ${tone(sc.diff)}`}>{n0(sc.diff)}</td>
                    <td className={`px-3 py-2 font-semibold ${sc.ratio == null ? '' : sc.ratio >= 1 ? 'text-secondary' : 'text-error'}`}>
                      {sc.ratio != null ? sc.ratio.toFixed(2) : '—'}
                    </td>
                    <td className="px-3 py-2"><SignalText score={sc.option} /></td>
                    <td className="px-3 py-2 text-on-surface">{fmt(r.spot)}</td>
                    <td className="px-3 py-2 text-on-surface">{r.fut_ltp != null ? fmt(r.fut_ltp) : '—'}</td>
                    <td className="px-3 py-2 text-on-surface">{r.fut_vwap != null ? fmt(r.fut_vwap) : '—'}</td>
                    <td className="px-3 py-2"><SignalText score={sc.vwap} /></td>
                    <td className="px-3 py-2"><OverallBadge score={sc.overall} /></td>
                  </tr>
                );
              })}
              {trend && trend.rows.length === 0 && (
                <tr>
                  <td colSpan={11} className="px-3 py-8 text-center text-on-surface-variant">
                    No snapshots yet — data is collected every minute from 09:15 to 15:30 IST.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* Live option chain */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm overflow-hidden">
        <div className="p-4 border-b border-outline-variant bg-surface-container-low">
          <h3 className="font-bold text-on-surface text-sm">Option Chain — latest poll</h3>
          <p className="text-[11px] text-on-surface-variant mt-0.5">
            Highlighted rows are the strikes counted in the trend above. ATM is bold.
          </p>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full text-xs tabular-nums">
            <thead className="text-on-surface-variant">
              <tr className="border-b border-outline-variant">
                {['Call OI', 'Call ΔOI', 'Call LTP', 'Call IV', 'Strike', 'Put IV', 'Put LTP', 'Put ΔOI', 'Put OI'].map((h) => (
                  <th key={h} className="px-3 py-2.5 font-semibold text-center whitespace-nowrap">{h}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {chainRows.map((r) => {
                const atm = r.strike === chain?.atm_strike;
                const cChg = r.call_oi - r.call_prev_oi;
                const pChg = r.put_oi - r.put_prev_oi;
                return (
                  <tr
                    key={r.strike}
                    className={`border-b border-outline-variant/50 text-center ${inWindow(r.strike) ? 'bg-primary/5' : ''} ${atm ? 'font-bold' : ''}`}
                  >
                    <td className="px-3 py-1.5 text-on-surface">{n0(r.call_oi)}</td>
                    <td className={`px-3 py-1.5 ${tone(cChg)}`}>{n0(cChg)}</td>
                    <td className="px-3 py-1.5 text-on-surface">{fmt(r.call_ltp)}</td>
                    <td className="px-3 py-1.5 text-on-surface-variant">{r.call_iv ? r.call_iv.toFixed(1) : '—'}</td>
                    <td className={`px-3 py-1.5 font-semibold ${atm ? 'text-primary' : 'text-on-surface'} bg-surface-container`}>{r.strike}</td>
                    <td className="px-3 py-1.5 text-on-surface-variant">{r.put_iv ? r.put_iv.toFixed(1) : '—'}</td>
                    <td className="px-3 py-1.5 text-on-surface">{fmt(r.put_ltp)}</td>
                    <td className={`px-3 py-1.5 ${tone(pChg)}`}>{n0(pChg)}</td>
                    <td className="px-3 py-1.5 text-on-surface">{n0(r.put_oi)}</td>
                  </tr>
                );
              })}
              {chainRows.length === 0 && (
                <tr>
                  <td colSpan={9} className="px-3 py-8 text-center text-on-surface-variant">
                    No chain loaded yet.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
