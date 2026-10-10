import { useEffect, useState } from 'react';
import { AlertTriangle, SlidersHorizontal } from 'lucide-react';
import { apiFetch } from '../lib/api';
import { fmt } from '../lib/format';

// ---------------------------------------------------------------------------
// Market data (Upstox option chain → OI / PCR / VWAP trend). Read-only: nothing
// on this screen changes anything. The signal settings it scores with are the
// server's (Settings → Telegram Call Rules), the same ones the engine sizes
// entries on when indicator sizing is ON — so the badges here show exactly what
// the engine would read.
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
// Signal settings (from the server's trading config)
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

interface ServerSignalConfig {
  indicator_sizing?: boolean;
  indicator_interval_min?: number;
  indicator_basis?: string;
  indicator_strike_window?: number;
  indicator_pcr_buy_above?: number;
  indicator_pcr_sell_below?: number;
  indicator_vwap_band_pct?: number;
}

/** Map the server's trading config onto the scoring settings; an older server
 * without these fields falls back to the defaults. */
function fromServer(c: ServerSignalConfig): SignalSettings {
  return sanitize({
    interval: c.indicator_interval_min,
    basis: c.indicator_basis === 'total' ? 'total' : 'chg',
    window: c.indicator_strike_window,
    pcrBuyAbove: c.indicator_pcr_buy_above,
    pcrSellBelow: c.indicator_pcr_sell_below,
    vwapBandPct: c.indicator_vwap_band_pct,
  });
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

function Setting({ label, value, hint }: { label: string; value: string; hint: string }) {
  return (
    <div className="flex flex-col gap-1 min-w-0">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-on-surface-variant">{label}</span>
      <span className="text-sm font-bold text-on-surface tabular-nums">{value}</span>
      <span className="text-[11px] text-on-surface-variant">{hint}</span>
    </div>
  );
}

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
  const serverCfg = usePolling<ServerSignalConfig>(serverBase, '/api/settings', 60_000);
  const s = serverCfg ? fromServer(serverCfg) : DEFAULT_SETTINGS;
  const sizingOn = serverCfg?.indicator_sizing === true;

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

      {/* Signal settings — read-only here, edited on the Settings page */}
      <div className="bg-surface-container-lowest rounded-xl border border-outline-variant shadow-sm p-4 space-y-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 className="flex items-center gap-2 font-bold text-on-surface text-sm">
            <SlidersHorizontal size={16} className="text-primary" /> Signal Settings
          </h3>
          <span
            className={`rounded-md px-2 py-0.5 text-[11px] font-bold ${
              sizingOn ? 'bg-secondary/15 text-secondary' : 'bg-surface-container text-on-surface-variant'
            }`}
          >
            Indicator sizing {sizingOn ? 'ON' : 'OFF'}
          </span>
        </div>
        <div className="grid grid-cols-2 sm:grid-cols-3 xl:grid-cols-6 gap-4">
          <Setting
            label="Interval"
            value={`${s.interval} mins`}
            hint="Checked 1 min after each boundary, first check 09:16."
          />
          <Setting
            label="OI basis"
            value={s.basis === 'chg' ? 'Change in OI' : 'Total OI'}
            hint="Change since yesterday's close, or total OI."
          />
          <Setting
            label="Strike range"
            value={WINDOWS.find((w) => w.value === s.window)?.label ?? `ATM ±${s.window}`}
            hint="Strikes each side of ATM counted."
          />
          <Setting label="PCR buy above" value={String(s.pcrBuyAbove)} hint="Option Signal = BUY (+1) above this." />
          <Setting label="PCR sell below" value={String(s.pcrSellBelow)} hint="Option Signal = SELL (−1) below this." />
          <Setting label="VWAP band" value={`±${s.vwapBandPct}%`} hint="VWAP Signal = 0 within this of VWAP." />
        </div>
        <p className="text-[11px] text-on-surface-variant border-t border-outline-variant pt-3">
          Overall score = Option Signal (+1 BUY / −1 SELL / 0) + VWAP Signal (+1 / −1 / 0), from −2 to +2. Above 0 is
          BUY, below 0 is SELL, and 0 is NEUTRAL.{' '}
          {sizingOn
            ? 'Indicator sizing is ON: when a Telegram call triggers, the engine reads the card badge (last closed check) — agrees scales the lots up, neutral scales them down, disagrees skips the call.'
            : 'Indicator sizing is OFF, so this is display only and never places or blocks a trade.'}{' '}
          These settings are edited in <span className="font-semibold">Settings → Telegram Call Rules</span>.
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
