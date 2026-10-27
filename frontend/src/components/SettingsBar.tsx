import { useCallback, useEffect, useRef, useState } from 'react';
import { Save, Settings, Sliders, Wallet, Layers3, Info, AlertTriangle, KeyRound, Lock, Gauge } from 'lucide-react';
import type { CallVerdict, IndicatorIndex, IndicatorPreview, TopupMode, TradingConfig } from '../types';
import { apiFetch } from '../lib/api';
import { useAuth } from '../context/AuthContext';
import { INDEX_LOT_REFERENCE } from '../lib/indexLots';

const CARD = 'bg-surface-container-lowest border border-outline-variant rounded-xl p-4 sm:p-6 shadow-sm';
const CARD_HEADER = 'flex items-center gap-2 text-base font-bold text-on-surface mb-4';
const FIELD_LABEL = 'text-label-caps text-on-surface-variant uppercase tracking-wide truncate';
/** Same look as FIELD_LABEL but wraps — for long labels (slider formulas) that
 * must stay readable on a phone instead of being cut off. */
const FIELD_LABEL_WRAP = 'text-label-caps text-on-surface-variant uppercase tracking-wide break-words';
const NUMBER_INPUT = 'w-full bg-surface border border-outline-variant rounded px-2.5 py-1.5 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all';

/// Small ⓘ button that toggles an explainer popover. The popover spans the
/// nearest `relative` ancestor (the setting's field container), so it never
/// overflows the viewport; a transparent full-screen backdrop closes it on any
/// outside click/tap, which also makes a second tap on the icon itself close it.
function InfoTip({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        aria-label="What does this setting do?"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="shrink-0 text-on-surface-variant/70 hover:text-primary transition-colors"
      >
        <Info size={12} />
      </button>
      {open && (
        <>
          <span className="fixed inset-0 z-20 cursor-default" onClick={() => setOpen(false)} />
          <span className="absolute inset-x-0 top-full mt-1 z-30 rounded-lg border border-outline-variant bg-surface-container-lowest p-2.5 text-[11px] leading-snug font-normal normal-case tracking-normal text-left text-on-surface shadow-lg whitespace-normal">
            {text}
          </span>
        </>
      )}
    </>
  );
}

function FactorSlider({ label, value, min, max, step, enabled, disabled, onChange, lowLabel, highLabel, decimals, disabledNote, info }: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  enabled: boolean;
  disabled?: boolean;
  onChange: (v: number) => void;
  lowLabel: string;
  highLabel: string;
  decimals?: number;
  disabledNote?: string;
  info?: string;
}) {
  const isInteractive = enabled && !disabled;
  return (
    <div className={`relative flex flex-col gap-1.5 min-w-0 sm:col-span-2 rounded-lg border border-outline-variant p-3.5 transition-opacity ${isInteractive ? 'bg-surface' : 'bg-surface opacity-50'}`}>
      <div className="flex items-start justify-between gap-3">
        <span className="flex items-center gap-1 min-w-0">
          <label className={FIELD_LABEL_WRAP}>{label}</label>
          {info && <InfoTip text={info} />}
        </span>
        <span className="shrink-0 text-sm font-mono-code font-bold text-primary tabular-nums">
          {value.toFixed(decimals ?? 2)}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={!isInteractive}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        className="w-full accent-primary disabled:cursor-not-allowed"
      />
      <div className="flex justify-between gap-3 text-[10px] text-on-surface-variant font-mono-code">
        <span className="min-w-0">{lowLabel}</span>
        <span className="min-w-0 text-right">{highLabel}</span>
      </div>
      {!enabled && (
        <span className="text-[11px] text-on-surface-variant italic">
          {disabledNote ?? 'Only applies when Dynamic Targeting is ON — and updates any already-open runner immediately on save.'}
        </span>
      )}
    </div>
  );
}

/// Number input that never silently turns a half-typed or emptied value into
/// 0: keystrokes commit only while they parse, blur clamps to [min, max], and
/// an empty / invalid field snaps back to the value it had on focus.
function NumberField({ label, info, value, onCommit, min, max, step, disabled, suffix }: {
  label: string;
  info: string;
  value: number;
  onCommit: (v: number) => void;
  min: number;
  max: number;
  step: number;
  disabled?: boolean;
  suffix?: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const atFocus = useRef(value);
  const clamp = (n: number) => Math.min(max, Math.max(min, n));
  return (
    <div className="relative flex flex-col justify-between gap-1.5 min-w-0">
      <span className="flex items-center gap-1 min-w-0">
        <label className={FIELD_LABEL_WRAP}>{label}</label>
        <InfoTip text={info} />
      </span>
      <div className="relative">
        <input
          type="number"
          inputMode="decimal"
          min={min}
          max={max}
          step={step}
          disabled={disabled}
          value={draft ?? String(value)}
          onFocus={() => {
            atFocus.current = value;
            setDraft(String(value));
          }}
          onChange={(e) => {
            setDraft(e.target.value);
            const n = parseFloat(e.target.value);
            if (Number.isFinite(n)) onCommit(n);
          }}
          onBlur={() => {
            const n = parseFloat(draft ?? '');
            onCommit(clamp(Number.isFinite(n) ? n : atFocus.current));
            setDraft(null);
          }}
          className={`${NUMBER_INPUT} ${suffix ? 'pr-9' : ''} ${disabled ? 'opacity-60 cursor-not-allowed' : ''}`}
        />
        {suffix && (
          <span className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-[11px] text-on-surface-variant">
            {suffix}
          </span>
        )}
      </div>
    </div>
  );
}

function Segmented<T extends string | number>({ options, value, onChange, disabled }: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
  disabled?: boolean;
}) {
  return (
    <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
      {options.map((o) => (
        <button
          key={String(o.value)}
          type="button"
          disabled={disabled}
          onClick={() => onChange(o.value)}
          className={`flex-1 min-w-0 px-2 py-1.5 transition-colors font-semibold whitespace-nowrap ${
            o.value === value
              ? 'bg-secondary text-on-secondary font-bold'
              : 'bg-surface text-on-surface-variant hover:bg-surface-container'
          } ${disabled ? 'opacity-60 cursor-not-allowed' : ''}`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function verdictText(v: CallVerdict): string {
  if (v.action === 'SKIP') return 'Skip';
  if (v.action === 'WAIT') return 'Wait (no buy)';
  if (v.base_lots <= 0) return 'Not traded (0 lots)';
  return `×${v.multiplier} → ${v.lots} lot${v.lots === 1 ? '' : 's'}`;
}

function IndexPreviewRow({ ix }: { ix: IndicatorIndex }) {
  const r = ix.reading;
  // A strong (±2) reading is solid, as on the Market Data page — it is the
  // one that sizes a call at the Strong multiplier.
  const strong = !!r && !r.opening && Math.abs(r.score.overall) >= 2;
  const tone = ix.bias === 'BULLISH'
    ? strong ? 'bg-secondary text-on-secondary' : 'bg-secondary/15 text-secondary'
    : ix.bias === 'BEARISH'
      ? strong ? 'bg-error text-on-error' : 'bg-error/15 text-error'
      : 'bg-surface-container text-on-surface-variant';
  const word = ix.bias === 'BULLISH' ? 'BUY' : ix.bias === 'BEARISH' ? 'SELL' : 'NEUTRAL';
  return (
    <li className="py-2.5 first:pt-0 last:pb-0 space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-bold text-on-surface">{ix.index}</span>
        <span className={`rounded-md px-2 py-0.5 text-[11px] font-bold tabular-nums whitespace-nowrap ${tone}`}>
          {!r
            ? 'No fresh reading → NEUTRAL'
            : r.opening
              ? `Opening check → NEUTRAL · ${r.check_time}`
              : `${r.score.overall > 0 ? '+' : ''}${r.score.overall} ${word} · ${r.check_time}`}
        </span>
      </div>
      <div className="grid grid-cols-2 gap-2">
        {([['BUY CE', ix.ce], ['BUY PE', ix.pe]] as const).map(([label, v]) => (
          <div
            key={label}
            title={v.note}
            className={`rounded-md border px-2 py-1.5 min-w-0 ${
              v.action !== 'SCALE' ? 'border-error/40 bg-error/5' : 'border-outline-variant bg-surface-container-lowest'
            }`}
          >
            <div className="text-[10px] font-semibold uppercase tracking-wide text-on-surface-variant">{label}</div>
            <div className={`text-xs font-bold tabular-nums ${v.action !== 'SCALE' ? 'text-error' : 'text-on-surface'}`}>
              {verdictText(v)}
            </div>
          </div>
        ))}
      </div>
    </li>
  );
}

/// What the engine would do with a call triggering right now — straight from
/// `GET /api/market/indicator`, i.e. the engine's own code and saved settings.
function IndicatorPreviewPanel({ preview }: { preview: IndicatorPreview }) {
  return (
    <div className="rounded-lg border border-outline-variant bg-surface p-3.5 space-y-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-on-surface-variant">
          If a call triggered now
        </span>
        <span className="text-[11px] font-mono-code text-on-surface-variant">{preview.now.slice(11, 16)} IST</span>
      </div>
      {!preview.enabled && (
        <p className="text-[11px] text-on-surface-variant italic">
          Indicator sizing is OFF — calls are bought at their default lots. This shows what it would do if ON.
        </p>
      )}
      {!preview.configured && (
        <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-2.5 py-2 text-[11px] text-amber-700">
          Upstox market data is not configured on the server, so there is never a fresh reading — every call would be
          sized as NEUTRAL.
        </p>
      )}
      <ul className="divide-y divide-outline-variant/60">
        {preview.indices.map((ix) => <IndexPreviewRow key={ix.index} ix={ix} />)}
      </ul>
      <p className="text-[10px] leading-snug text-on-surface-variant">
        FINNIFTY, MIDCPNIFTY, BANKEX and stock options follow the NIFTY reading. Uses saved settings — save to refresh.
      </p>
    </div>
  );
}

const INDICATOR_INTERVALS = [5, 15, 30, 60];
/** The Aggressive Top-Up rules, least buying first (backend `entry_gate::TopupMode`). */
const TOPUP_RULES: { value: TopupMode; label: string; detail: string }[] = [
  {
    value: 'strong_to_agree',
    label: 'Strong agree → Agrees size',
    detail: 'Only a strong agree (+2 / −2) tops up, and only to the Agrees size. Once per call. Buys the least.',
  },
  {
    value: 'strong_to_strong',
    label: 'Strong agree → Strong size',
    detail: 'Only a strong agree tops up, straight to the Strong size. Once per call.',
  },
  {
    value: 'tier',
    label: 'Any agree → its own size',
    detail: 'A plain agree tops up to the Agrees size, a strong agree to the Strong size — whichever reads first. Once per call.',
  },
  {
    value: 'step',
    label: 'Step up',
    detail: 'As above, then once more to the Strong size if the reading later turns strong. A call bought at the Agrees size steps up too. Up to two top-ups per call.',
  },
];
const INDICATOR_WINDOWS = [
  { value: 5, label: '±5' },
  { value: 10, label: '±10' },
  { value: 20, label: '±20' },
  { value: 0, label: 'All' },
];

/// Telegram call rules: how long a call stays valid, and VWAP + PCR indicator
/// sizing at the moment of buying (see backend `shared_domain::entry_gate`).
function CallRulesCard({ cfg, setCfg, disabled, preview }: {
  cfg: TradingConfig;
  setCfg: (fn: (c: TradingConfig | null) => TradingConfig | null) => void;
  disabled: boolean;
  preview: IndicatorPreview | null;
}) {
  const set = (patch: Partial<TradingConfig>) => setCfg((c) => c && { ...c, ...patch });
  const on = cfg.indicator_sizing;
  return (
    <div className={CARD}>
      <div className={`${CARD_HEADER} relative`}>
        <Gauge size={16} className="text-primary" /> Telegram Call Rules
        <InfoTip text="How long a Telegram call stays valid, and how the VWAP + PCR indicator sizes it at the moment it is bought. Both apply in PAPER and LIVE." />
      </div>

      <div className="grid grid-cols-1 gap-5 lg:grid-cols-2">
        <div className="space-y-5 min-w-0">
          {/* Call validity — independent of the indicator toggle */}
          <div className="grid grid-cols-2 gap-3.5 items-end">
            <NumberField
              label="Call Valid For (min)"
              info="A call that hasn't been bought within this many minutes of arriving is dropped (logged as CALL_EXPIRED). Only calls not yet bought — an entry already sent to the broker and every open position are untouched. Cancelling it yourself still works any time. 0 = off (valid until the 15:39 cutoff)."
              value={cfg.call_validity_minutes}
              onCommit={(v) => set({ call_validity_minutes: Math.round(v) })}
              min={0}
              max={390}
              step={5}
              suffix="min"
              disabled={disabled}
            />
            <p className="pb-1.5 text-[11px] leading-snug text-on-surface-variant">
              {cfg.call_validity_minutes > 0
                ? `= ${Math.floor(cfg.call_validity_minutes / 60)}h ${String(cfg.call_validity_minutes % 60).padStart(2, '0')}m after the call`
                : 'Off — valid until 15:39'}
            </p>
          </div>

          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Indicator Sizing (VWAP + PCR)</label>
              <InfoTip text="ON: when a call triggers, read the last closed VWAP + PCR check of its index (BANKNIFTY and SENSEX their own; NIFTY for everything else). Strongly agrees with the call — PCR and VWAP both on its side, +2 for BUY CE or −2 for BUY PE → default lots × the strong multiplier. Agrees — one of them on its side and the other neutral, +1 / −1 → × the agree multiplier. NEUTRAL or no fresh reading → × the neutral multiplier. Disagrees → the call is not bought: it keeps waiting or is dropped, per the switch below, and the log says why. A manual qty override is never scaled, but is still held back on disagreement. OFF: every call is bought at its default lots." />
            </span>
            <Segmented
              options={[{ value: 1, label: 'ON' }, { value: 0, label: 'OFF' }]}
              value={on ? 1 : 0}
              onChange={(v) => set({ indicator_sizing: v === 1 })}
              disabled={disabled}
            />
          </div>

          <div className={`space-y-4 transition-opacity ${on ? '' : 'opacity-50'}`}>
            <div className="grid grid-cols-3 gap-2.5">
              <NumberField
                label="Strong ×"
                info="Lot multiplier when the indicator strongly agrees with the call: PCR and VWAP both on its side (+2 for BUY CE, −2 for BUY PE). Lots round to the nearest whole lot (exact halves round down), never below 1 — so 1 lot × 1.5 stays 1 lot, 2 lots × 1.5 → 3."
                value={cfg.indicator_strong_agree_multiplier}
                onCommit={(v) => set({ indicator_strong_agree_multiplier: v })}
                min={0.1}
                max={5}
                step={0.1}
                disabled={disabled}
              />
              <NumberField
                label="Agrees ×"
                info="Lot multiplier when the indicator agrees with the call on one signal only: PCR or VWAP on its side, the other neutral (+1 for BUY CE, −1 for BUY PE). Set 1 to buy the default lots. Same rounding, never below 1 lot."
                value={cfg.indicator_agree_multiplier}
                onCommit={(v) => set({ indicator_agree_multiplier: v })}
                min={0.1}
                max={5}
                step={0.1}
                disabled={disabled}
              />
              <NumberField
                label="Neutral ×"
                info="Lot multiplier when the indicator is NEUTRAL — or has no fresh reading (Upstox down, before the 09:16 first check). Same rounding, never below 1 lot."
                value={cfg.indicator_neutral_multiplier}
                onCommit={(v) => set({ indicator_neutral_multiplier: v })}
                min={0.1}
                max={5}
                step={0.1}
                disabled={disabled}
              />
            </div>
            <p className="text-[11px] leading-snug text-on-surface-variant">
              <span className="font-semibold text-error">
                Disagrees → {cfg.indicator_wait_on_disagree ? 'wait, no buy' : 'skip'}.
              </span>{' '}
              Lots round to the nearest whole lot, exact halves down, minimum 1.
            </p>

            <div className="space-y-3.5 border-t border-outline-variant/60 pt-4">
              {([
                {
                  key: 'indicator_early_checks',
                  label: 'Opening Checks (09:16 neutral, then 5 min)',
                  info: 'ON: the 09:16 check always counts as NEUTRAL. From 09:22 the engine uses the latest closed 5-minute check (09:21, 09:26) until the first regular check (09:31 on 15 or 30 min, 10:01 on 60 min), then goes back to the interval below. OFF: the 09:16 check is scored like any other and is the only reading until the first regular check.',
                },
                {
                  key: 'indicator_wait_on_disagree',
                  label: 'When It Disagrees: Keep Waiting',
                  info: 'ON: a call whose price is in the buy range while the indicator disagrees is not bought and not dropped. It is re-read continuously and bought as soon as the indicator turns neutral or agrees with the price still in the buy range. It still expires with Call Valid For and the 15:39 cutoff. OFF: the call is dropped the moment it triggers against the indicator.',
                },
                {
                  key: 'indicator_topup',
                  label: 'Aggressive Top-Up',
                  info: 'ON: a call bought at the reduced Neutral size is topped up if the indicator later agrees while the price is back in the buy range. Which reading does it, and up to what size, is the Top-Up Rule below. Only before target 1, only while the stop is still at the signal level and nothing has been sold, never after Call Valid For or 15:39, never on a manual qty override. The added lots share the same stop-loss and targets. The indicator never sells: if it turns against an open position nothing happens. OFF: a position keeps the size it was bought at.',
                },
              ] as const).map((t) => (
                <div key={t.key} className="relative flex flex-col gap-1.5">
                  <span className="flex items-center gap-1 min-w-0">
                    <label className={FIELD_LABEL}>{t.label}</label>
                    <InfoTip text={t.info} />
                  </span>
                  <Segmented
                    options={[{ value: 1, label: 'ON' }, { value: 0, label: 'OFF' }]}
                    value={cfg[t.key] ? 1 : 0}
                    onChange={(v) => set({ [t.key]: v === 1 })}
                    disabled={disabled}
                  />
                </div>
              ))}

              {/* Sub-option of Aggressive Top-Up: which reading tops up, and how far. */}
              <div className={`relative flex flex-col gap-1.5 transition-opacity ${cfg.indicator_topup ? '' : 'opacity-50'}`}>
                <span className="flex items-center gap-1 min-w-0">
                  <label className={FIELD_LABEL}>Top-Up Rule</label>
                  <InfoTip text="Which indicator reading tops up a call bought at the Neutral size, and up to which size. Sizes are the default lots × the multipliers above: Agrees size = × Agrees, Strong size = × Strong. Every rule keeps the other top-up limits: before target 1, stop untouched, nothing sold, inside Call Valid For and 15:39." />
                </span>
                <div className="flex flex-col gap-1.5">
                  {TOPUP_RULES.map((r) => {
                    const selected = cfg.indicator_topup_mode === r.value;
                    const locked = disabled || !cfg.indicator_topup;
                    return (
                      <button
                        key={r.value}
                        type="button"
                        aria-pressed={selected}
                        disabled={locked}
                        onClick={() => set({ indicator_topup_mode: r.value })}
                        className={`rounded-lg border px-3 py-2 text-left transition-colors ${
                          selected
                            ? 'border-secondary bg-secondary/10'
                            : 'border-outline-variant bg-surface hover:bg-surface-container'
                        } ${locked ? 'cursor-not-allowed' : ''}`}
                      >
                        <span className={`block text-xs font-semibold ${selected ? 'text-secondary' : 'text-on-surface'}`}>
                          {r.label}
                        </span>
                        <span className="block text-[11px] leading-snug text-on-surface-variant">{r.detail}</span>
                      </button>
                    );
                  })}
                </div>
                {!cfg.indicator_topup && (
                  <span className="text-[11px] text-on-surface-variant italic">
                    Only applies when Aggressive Top-Up is ON.
                  </span>
                )}
              </div>
            </div>
          </div>

          {/* Lower-SL gate — independent of the Indicator Sizing toggle */}
          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Lower SL Only on Strong Indicator</label>
              <InfoTip text="ON: when the channel moves a call's stop-loss lower (a 'move SL to …' reply, an edited call or a resent call with a lower SL), the engine reads the call's index indicator at that moment. The lower SL is applied only if the indicator strongly agrees with the call: +2 for BUY CE, −2 for BUY PE (PCR and VWAP both on its side). Anything less (+1, NEUTRAL, the 09:16 opening check, no fresh reading, or against the call) and the stop stays where it is, logged as SL_LOWER_REFUSED. It is not re-checked later. Moving the stop up is always applied. Works whether Indicator Sizing is ON or OFF, using the indicator settings below. OFF: every SL move from the channel is applied, as before." />
            </span>
            <Segmented
              options={[{ value: 1, label: 'ON' }, { value: 0, label: 'OFF' }]}
              value={cfg.indicator_sl_lower_gate ? 1 : 0}
              onChange={(v) => set({ indicator_sl_lower_gate: v === 1 })}
              disabled={disabled}
            />
            <p className="text-[11px] leading-snug text-on-surface-variant">
              {cfg.indicator_sl_lower_gate
                ? 'A lower SL needs +2 for CE / −2 for PE, otherwise the stop is kept. A higher SL is always applied.'
                : 'Every SL move from the channel is applied.'}
            </p>
          </div>

          {/* Indicator parameters — also what the Market Data page scores with */}
          <div className="space-y-3.5 rounded-lg border border-outline-variant p-3.5">
            <div className="flex items-center gap-1 text-[11px] font-semibold uppercase tracking-wide text-on-surface-variant">
              Indicator settings
              <InfoTip text="How the VWAP + PCR score is computed — the same settings the Market Data page scores with, so its badges show exactly what the engine sizes on. Overall = Option Signal (PCR: +1 BUY / −1 SELL / 0) + VWAP Signal (future vs VWAP: +1 / −1 / 0); above 0 is BUY, below 0 SELL, 0 NEUTRAL." />
            </div>
            <div className="relative flex flex-col gap-1.5">
              <label className={FIELD_LABEL}>Check interval</label>
              <Segmented
                options={INDICATOR_INTERVALS.map((m) => ({ value: m, label: `${m}m` }))}
                value={cfg.indicator_interval_min}
                onChange={(v) => set({ indicator_interval_min: v })}
                disabled={disabled}
              />
            </div>
            <div className="grid grid-cols-1 gap-3.5 sm:grid-cols-2">
              <div className="relative flex flex-col gap-1.5 min-w-0">
                <label className={FIELD_LABEL}>OI basis</label>
                <Segmented
                  options={[
                    { value: 'chg' as const, label: 'Change' },
                    { value: 'total' as const, label: 'Total' },
                  ]}
                  value={cfg.indicator_basis}
                  onChange={(v) => set({ indicator_basis: v })}
                  disabled={disabled}
                />
              </div>
              <div className="relative flex flex-col gap-1.5 min-w-0">
                <label className={FIELD_LABEL}>Strike range (ATM)</label>
                <Segmented
                  options={INDICATOR_WINDOWS}
                  value={cfg.indicator_strike_window}
                  onChange={(v) => set({ indicator_strike_window: v })}
                  disabled={disabled}
                />
              </div>
            </div>
            <div className="grid grid-cols-3 gap-2.5">
              <NumberField
                label="PCR buy >"
                info="Option Signal = BUY (+1) when PCR is above this."
                value={cfg.indicator_pcr_buy_above}
                onCommit={(v) => set({
                  indicator_pcr_buy_above: v,
                  // The sell threshold can never sit above the buy one (the server clamps it too).
                  indicator_pcr_sell_below: Math.min(cfg.indicator_pcr_sell_below, v),
                })}
                min={0}
                max={10}
                step={0.05}
                disabled={disabled}
              />
              <NumberField
                label="PCR sell <"
                info="Option Signal = SELL (−1) when PCR is below this (never above the buy threshold). Between the two = 0."
                value={cfg.indicator_pcr_sell_below}
                onCommit={(v) => set({ indicator_pcr_sell_below: v })}
                min={0}
                max={cfg.indicator_pcr_buy_above}
                step={0.05}
                disabled={disabled}
              />
              <NumberField
                label="VWAP band"
                info="VWAP Signal = 0 while the index future is within ± this % of its VWAP."
                value={cfg.indicator_vwap_band_pct}
                onCommit={(v) => set({ indicator_vwap_band_pct: v })}
                min={0}
                max={5}
                step={0.01}
                suffix="%"
                disabled={disabled}
              />
            </div>
          </div>
        </div>

        <div className="min-w-0">
          {preview ? (
            <IndicatorPreviewPanel preview={preview} />
          ) : (
            <div className="rounded-lg border border-dashed border-outline-variant p-3.5 text-[11px] text-on-surface-variant">
              Live preview unavailable — the server hasn&apos;t been updated to a version with indicator sizing yet.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export function SettingsBar({ serverBase }: { serverBase: string }) {
  const { hasWriteAccess, openUnlockModal, lock } = useAuth();
  const [cfg, setCfg] = useState<TradingConfig | null>(null);
  const [virtualBalance, setVirtualBalance] = useState<number>(0);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [preview, setPreview] = useState<IndicatorPreview | null>(null);

  const fields: { key: keyof TradingConfig | 'virtual_balance'; label: string; info: string }[] = [
    { key: 'virtual_balance', label: 'Virtual Balance (₹)',
      info: 'Starting cash for PAPER mode — simulated buys, sells and their fees all settle against this wallet. Not used in LIVE mode.' },
    { key: 'index_lots', label: 'Index Lots',
      info: 'Default lots to buy for an index option signal (NIFTY, SENSEX, …) when that index has no per-index override below. 0 = skip index signals that have no override.' },
    { key: 'other_lots', label: 'Other Lots',
      info: 'Default lots to buy for a stock option signal. 0 = don’t auto-trade stock options at all.' },
    { key: 'brokerage_per_order', label: 'Brokerage (₹)',
      info: 'Flat brokerage per order leg used in the P&L fee breakdown (Kotak Neo charges a flat ₹20). Changes what P&L reports show, not what the broker actually charges.' },
    { key: 'max_trade_amount_inr', label: 'Max Trade (₹)',
      info: 'Sizes non-option (equity) signals only: quantity ≈ this amount ÷ price, rounded down to whole lots. Option trades are sized by the lot settings, not by this.' },
    { key: 'target_1_exit_pct', label: 'Target 1 Exit %',
      info: 'Share of the position sold when target 1 hits, rounded up to whole lots. Ignored when Dynamic Targeting is ON — that always sells exactly one lot at target 1 (or nothing on a one-lot position, if Dynamic Targeting — 1 Lot is ON).' },
    { key: 'target_2_exit_pct', label: 'Target 2 Exit %',
      info: 'Reserved — the engine currently always exits the entire remaining position when target 2 hits, whatever this is set to.' },
    { key: 'entry_market_protection', label: 'Entry MP %',
      info: 'Kotak market-price protection on LIVE entry buys: the order can fill at most this % above the trigger price, so a spike can’t fill you arbitrarily worse. Protective exits always use 0.' },
    { key: 'entry_window_pct', label: 'Entry Window %',
      info: 'Refuses to chase a runaway entry. As a % of the entry→target-1 distance: for "ABOVE X" the engine only buys between X and X + this%×(target1−X); for "BELOW X", between X − that and X. If the price has already blown past the far edge, the trade waits for a pull-back into the window and expires unfilled at the 15:39 no-entry cutoff if it never comes. 0 = off (buy the instant the trigger is crossed). Applies in PAPER and LIVE.' },
  ];

  const loadSettings = useCallback(() => {
    return Promise.all([
      apiFetch(serverBase, '/api/settings').then((r) => r.json()),
      apiFetch(serverBase, '/api/wallet/balance').then((r) => r.json()),
    ])
      .then(([cfgData, walletData]) => {
        // An older backend (mid-deploy skew) won't send these fields at all —
        // default them so every read below can assume they're present.
        setCfg({
          ...cfgData,
          index_lots_by_symbol: cfgData?.index_lots_by_symbol ?? {},
          dynamic_targeting_trail_factor: cfgData?.dynamic_targeting_trail_factor ?? 0.5,
          dynamic_targeting_extension_factor: cfgData?.dynamic_targeting_extension_factor ?? 1.0,
          dynamic_targeting_single_lot: cfgData?.dynamic_targeting_single_lot ?? false,
          pre_t1_trailing: cfgData?.pre_t1_trailing ?? false,
          pre_t1_trail_arm_pct: cfgData?.pre_t1_trail_arm_pct ?? 60,
          pre_t1_trail_factor: cfgData?.pre_t1_trail_factor ?? 0.5,
          entry_window_pct: cfgData?.entry_window_pct ?? 0,
          indicator_sizing: cfgData?.indicator_sizing ?? false,
          indicator_agree_multiplier: cfgData?.indicator_agree_multiplier ?? 1.0,
          indicator_strong_agree_multiplier: cfgData?.indicator_strong_agree_multiplier ?? 1.5,
          indicator_neutral_multiplier: cfgData?.indicator_neutral_multiplier ?? 0.5,
          indicator_interval_min: cfgData?.indicator_interval_min ?? 15,
          indicator_basis: cfgData?.indicator_basis === 'total' ? 'total' : 'chg',
          indicator_strike_window: cfgData?.indicator_strike_window ?? 10,
          indicator_pcr_buy_above: cfgData?.indicator_pcr_buy_above ?? 1,
          indicator_pcr_sell_below: cfgData?.indicator_pcr_sell_below ?? 1,
          indicator_vwap_band_pct: cfgData?.indicator_vwap_band_pct ?? 0,
          call_validity_minutes: cfgData?.call_validity_minutes ?? 120,
          indicator_early_checks: cfgData?.indicator_early_checks ?? true,
          indicator_wait_on_disagree: cfgData?.indicator_wait_on_disagree ?? true,
          indicator_topup: cfgData?.indicator_topup ?? false,
          indicator_topup_mode: cfgData?.indicator_topup_mode ?? 'strong_to_agree',
          indicator_sl_lower_gate: cfgData?.indicator_sl_lower_gate ?? false,
        });
        setVirtualBalance(typeof walletData?.balance === 'number' ? walletData.balance : 0);
      })
      .catch(console.error);
  }, [serverBase]);

  const loadPreview = useCallback(() => {
    apiFetch(serverBase, '/api/market/indicator')
      .then((r) => (r.ok ? r.json() : null))
      .then((d: IndicatorPreview | null) => setPreview(d && Array.isArray(d.indices) ? d : null))
      .catch(() => setPreview(null));
  }, [serverBase]);

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  // The preview follows the market minute by minute, so keep it fresh.
  useEffect(() => {
    loadPreview();
    const timer = setInterval(loadPreview, 30_000);
    return () => clearInterval(timer);
  }, [loadPreview]);

  async function handleSave() {
    if (!cfg) return;
    if (!hasWriteAccess) {
      openUnlockModal('Saving settings requires Write Access');
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const [settingsRes, walletRes] = await Promise.all([
        apiFetch(serverBase, '/api/settings', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(cfg),
        }),
        apiFetch(serverBase, '/api/wallet/balance', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ balance: virtualBalance }),
        }),
      ]);
      // A rejected save used to still flash "Saved" — never claim a setting
      // that real orders depend on was stored when it wasn't.
      if (!settingsRes.ok || !walletRes.ok) {
        setSaveError(`Save failed (HTTP ${!settingsRes.ok ? settingsRes.status : walletRes.status}) — nothing changed on the server.`);
        return;
      }
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
      // Show what the server actually stored (it clamps out-of-range values)
      // and what the indicator now says with the new settings.
      await loadSettings();
      loadPreview();
    } catch (e) {
      console.error(e);
      setSaveError('Save failed — could not reach the server. Nothing was confirmed as saved.');
    } finally {
      setSaving(false);
    }
  }

  if (!cfg) {
    return (
      <div className={`${CARD} flex items-center gap-2 text-on-surface-variant text-sm`}>
        <Settings size={14} className="animate-spin text-primary" /> Loading settings…
      </div>
    );
  }

  function setIndexLots(symbol: string, raw: string) {
    if (!hasWriteAccess) return;
    setCfg((c) => {
      if (!c) return c;
      const next = { ...c.index_lots_by_symbol };
      const n = parseInt(raw, 10);
      // Empty, non-numeric, or 0-and-below all mean "reset to Auto" — typing
      // 0 is a natural way to clear an override, so it must not be a no-op.
      if (raw === '' || Number.isNaN(n) || n <= 0) {
        delete next[symbol];
      } else {
        next[symbol] = n;
      }
      return { ...c, index_lots_by_symbol: next };
    });
  }

  return (
    <div className="space-y-6">
      {/* Read-Only Mode Notice Banner */}
      {!hasWriteAccess && (
        <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-3 p-3.5 bg-amber-500/10 border border-amber-500/30 rounded-xl text-xs text-amber-400 font-medium shadow-sm">
          <div className="flex items-center gap-2">
            <AlertTriangle size={16} className="shrink-0 text-amber-400" />
            <span>
              <strong>Read-Only Mode:</strong> You are viewing live settings. Sign in as the admin to modify configuration or save changes.
            </span>
          </div>
          <button
            type="button"
            onClick={() => openUnlockModal('Sign in as the admin to edit trading settings')}
            className="shrink-0 flex items-center gap-1.5 px-3 py-1.5 bg-amber-500/20 hover:bg-amber-500/30 text-amber-300 rounded-lg text-xs font-bold transition-colors"
          >
            <KeyRound size={13} />
            Unlock to Edit
          </button>
        </div>
      )}

      {/* Trading mode + risk controls */}
      <div className={CARD}>
        <div className={CARD_HEADER}>
          <Sliders size={16} className="text-primary" /> Trading Mode &amp; Risk
        </div>

        {/* grid-cols-1 = minmax(0, 1fr): without an explicit column template the
            single mobile column grew to fit the longest slider label (~600px)
            and the whole settings page panned sideways on a phone. */}
        <div className="grid grid-cols-1 gap-5 sm:grid-cols-2">
          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Trading Mode</label>
              <InfoTip text="PAPER simulates entries and exits at the live market price against the virtual wallet — no real orders. LIVE places real orders on your Kotak Neo account with real money." />
            </span>
            <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
              {(['PAPER', 'LIVE'] as const).map((m) => (
                <button
                  key={m}
                  disabled={!hasWriteAccess}
                  onClick={() => {
                    if (!hasWriteAccess) {
                      openUnlockModal('Changing trading mode requires Write Access');
                      return;
                    }
                    setCfg((c) => c && { ...c, mode: m });
                  }}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.mode === m
                      ? m === 'LIVE'
                        ? 'bg-error text-on-error font-bold'
                        : 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  } ${!hasWriteAccess ? 'opacity-60 cursor-not-allowed' : ''}`}
                >
                  {m}
                </button>
              ))}
            </div>
          </div>

          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Dynamic Targeting</label>
              <InfoTip text="ON: sell one lot at target 1, then let the rest run with no fixed target 2 — each rung climbed trails the stop up and extends the next rung, and the runner only exits when price falls back through the trailed stop. OFF: the runner exits at the signal's fixed target 2." />
            </span>
            <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
              {([true, false] as const).map((on) => (
                <button
                  key={String(on)}
                  disabled={!hasWriteAccess}
                  onClick={() => {
                    if (!hasWriteAccess) {
                      openUnlockModal('Changing dynamic targeting requires Write Access');
                      return;
                    }
                    setCfg((c) => c && { ...c, dynamic_targeting: on });
                  }}
                  title={on ? 'Sell one lot at target 1, then trail an extending target ladder for the runner' : "Exit the runner at the signal's fixed target 2 (default)"}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.dynamic_targeting === on
                      ? 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  } ${!hasWriteAccess ? 'opacity-60 cursor-not-allowed' : ''}`}
                >
                  {on ? 'ON' : 'OFF'}
                </button>
              ))}
            </div>
          </div>

          {/* One-lot runner — sub-option of Dynamic Targeting. A one-lot
              position otherwise sells its only lot at target 1 and is flat. */}
          <div className={`relative flex flex-col gap-1.5 transition-opacity ${cfg.dynamic_targeting ? '' : 'opacity-50'}`}>
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Dynamic Targeting — 1 Lot</label>
              <InfoTip text="With Dynamic Targeting ON, a position of only one lot normally sells that lot at target 1 and is done. ON: sell nothing at target 1 — the whole lot becomes the runner, its stop trails up and the next rung extends exactly like a multi-lot runner, and it only exits when price falls back through the trailed stop (which never sits below entry). Positions of 2+ lots are unaffected. Applies in PAPER and LIVE." />
            </span>
            <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
              {([true, false] as const).map((on) => (
                <button
                  key={String(on)}
                  disabled={!hasWriteAccess || !cfg.dynamic_targeting}
                  onClick={() => {
                    if (!hasWriteAccess) {
                      openUnlockModal('Changing dynamic targeting requires Write Access');
                      return;
                    }
                    setCfg((c) => c && { ...c, dynamic_targeting_single_lot: on });
                  }}
                  title={on
                    ? 'One-lot positions keep their whole lot as a dynamic runner at target 1 instead of exiting'
                    : 'One-lot positions exit in full at target 1 (default)'}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.dynamic_targeting_single_lot === on
                      ? 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  } ${!hasWriteAccess || !cfg.dynamic_targeting ? 'opacity-60 cursor-not-allowed' : ''}`}
                >
                  {on ? 'ON' : 'OFF'}
                </button>
              ))}
            </div>
            {!cfg.dynamic_targeting && (
              <span className="text-[11px] text-on-surface-variant italic">
                Only applies when Dynamic Targeting is ON.
              </span>
            )}
          </div>

          <FactorSlider
            label="Trail Factor (stop = rung − diff × factor)"
            value={cfg.dynamic_targeting_trail_factor}
            min={0}
            max={1}
            step={0.05}
            enabled={cfg.dynamic_targeting}
            disabled={!hasWriteAccess}
            onChange={(v) => setCfg((c) => c && { ...c, dynamic_targeting_trail_factor: v })}
            lowLabel="0.00 — tightest (locks the rung)"
            highLabel="1.00 — loosest (breakeven on rung 1)"
            info="How far the runner's stop sits below the last rung hit: stop = rung − (target1 − entry) × factor. Lower = tighter (locks in more, shaken out sooner); 1.00 = loosest (breakeven after the first rung). Saving updates any open runner immediately."
          />

          <FactorSlider
            label="Extension Factor (next rung = rung + diff × factor)"
            value={cfg.dynamic_targeting_extension_factor}
            min={0.1}
            max={3}
            step={0.1}
            enabled={cfg.dynamic_targeting}
            disabled={!hasWriteAccess}
            onChange={(v) => setCfg((c) => c && { ...c, dynamic_targeting_extension_factor: v })}
            lowLabel="0.10 — rungs packed close together"
            highLabel="3.00 — rungs spread far apart"
            info="How far away the next rung is placed after one is hit: next rung = rung + (target1 − entry) × factor. 1.00 keeps the signal's original target-1 spacing; smaller trails the stop more often, larger waits longer between ratchets. Saving updates any open runner immediately."
          />

          {/* Pre-T1 trailing — protect gains when price nears target 1 but
              reverses before touching it, without capping trades that do get
              there (target 1 and everything after behave exactly as before). */}
          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Pre-T1 Trailing Stop</label>
              <InfoTip text="Protects gains before target 1: tracks the peak price since entry and, once armed, ratchets the stop up behind the peak — so a reversal just short of target 1 exits in profit instead of riding back to the original SL. Trades that do reach target 1 behave exactly as if this were OFF. Works in PAPER and LIVE." />
            </span>
            <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
              {([true, false] as const).map((on) => (
                <button
                  key={String(on)}
                  disabled={!hasWriteAccess}
                  onClick={() => {
                    if (!hasWriteAccess) {
                      openUnlockModal('Changing pre-T1 trailing requires Write Access');
                      return;
                    }
                    setCfg((c) => c && { ...c, pre_t1_trailing: on });
                  }}
                  title={on
                    ? 'Before target 1: once price covers the arm % of the way there, trail the stop below the peak so a near-miss reversal exits in profit instead of riding back to the original SL'
                    : "Keep the signal's original SL untouched until target 1 hits (default)"}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.pre_t1_trailing === on
                      ? 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  } ${!hasWriteAccess ? 'opacity-60 cursor-not-allowed' : ''}`}
                >
                  {on ? 'ON' : 'OFF'}
                </button>
              ))}
            </div>
          </div>

          <FactorSlider
            label="Arm At (% of entry → target-1 distance)"
            value={cfg.pre_t1_trail_arm_pct}
            min={0}
            max={100}
            step={5}
            enabled={cfg.pre_t1_trailing}
            disabled={!hasWriteAccess}
            onChange={(v) => setCfg((c) => c && { ...c, pre_t1_trail_arm_pct: v })}
            lowLabel="0 — trails from entry"
            highLabel="100 — never arms before target 1"
            decimals={0}
            disabledNote="Only applies when Pre-T1 Trailing is ON. Works in PAPER and LIVE."
            info="How much of the entry → target-1 distance price must cover before the pre-T1 trail starts moving the stop (60 arms at entry + 60% of the way to target 1). Until then the signal's original SL keeps its full room, so ordinary noise near entry can't shake you out."
          />

          <FactorSlider
            label="Pre-T1 Trail Factor (stop = peak − diff × factor)"
            value={cfg.pre_t1_trail_factor}
            min={0}
            max={1}
            step={0.05}
            enabled={cfg.pre_t1_trailing}
            disabled={!hasWriteAccess}
            onChange={(v) => setCfg((c) => c && { ...c, pre_t1_trail_factor: v })}
            lowLabel="0.00 — tightest (exit on any dip)"
            highLabel="1.00 — loosest (breakeven at target 1)"
            disabledNote="Only applies when Pre-T1 Trailing is ON. Works in PAPER and LIVE."
            info="Trail distance once armed: stop = peak − (target1 − entry) × factor, never below the original SL and never moved back down. 0.00 exits on the first tick that isn't a new high; 1.00 only reaches breakeven when the peak touches target 1."
          />
        </div>
      </div>

      {/* Sizing + fees */}
      <div className={CARD}>
        <div className={CARD_HEADER}>
          <Wallet size={16} className="text-primary" /> Trade Sizing &amp; Fees
        </div>
        <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 gap-3.5">
          {fields
            .filter(({ key }) => key !== 'virtual_balance' || cfg.mode !== 'LIVE')
            .map(({ key, label, info }) => (
              <div key={key} className="relative flex flex-col justify-between gap-1.5 min-w-0">
                <span className="flex items-center gap-1 min-w-0">
                  <label className={FIELD_LABEL_WRAP}>{label}</label>
                  <InfoTip text={info} />
                </span>
                <input
                  type="number"
                  min={0}
                  disabled={!hasWriteAccess}
                  value={key === 'virtual_balance' ? String(virtualBalance) : String(cfg[key])}
                  onChange={(e) =>
                    key === 'virtual_balance'
                      ? setVirtualBalance(parseFloat(e.target.value) || 0)
                      : setCfg((c) => c && { ...c, [key]: parseFloat(e.target.value) || 0 })
                  }
                  className={`${NUMBER_INPUT} ${!hasWriteAccess ? 'opacity-60 cursor-not-allowed' : ''}`}
                />
              </div>
            ))}
        </div>
        <p className="mt-3 text-[11px] text-on-surface-variant">
          <span className="font-semibold">Index Lots</span> / <span className="font-semibold">Other Lots</span> set to
          {' '}<span className="font-mono-code">0</span> means don&apos;t auto-trade that class — index signals with no
          per-index lot below, and every stock-option signal, are skipped with a log line.
        </p>
      </div>

      {/* Per-index default lots — a trader who only trades indexes can size
          each one independently (e.g. 2 lots of SENSEX, 1 of MIDCPNIFTY)
          instead of one "Index Lots" number applied to all of them. */}
      <div className={CARD}>
        <div className={`${CARD_HEADER} relative`}>
          <Layers3 size={16} className="text-primary" /> Default Lots by Index
          <InfoTip text="Per-index override for Index Lots — e.g. 2 lots of SENSEX but 1 of NIFTY, reflecting their different lot sizes. Blank = use the Index Lots default; if that default is 0, a blank index is skipped entirely. The line under each box shows the resulting quantity (lots × lot size)." />
        </div>
        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-6 gap-2.5">
          {INDEX_LOT_REFERENCE.map(({ symbol, label, lotSize }) => {
            const configured = cfg.index_lots_by_symbol[symbol];
            // A per-index box left blank falls back to "Index Lots"; that
            // fallback can itself be 0, which means this index is skipped
            // unless it gets its own positive lot count here.
            const effectiveLots = configured && configured > 0 ? configured : cfg.index_lots;
            return (
              <div key={symbol} className="flex flex-col gap-1 bg-surface border border-outline-variant rounded-lg p-2.5">
                <div className="flex items-baseline justify-between gap-1">
                  <span className="text-xs font-bold text-on-surface truncate" title={label}>{symbol}</span>
                  <span className="text-[10px] text-on-surface-variant font-mono-code shrink-0">lot {lotSize}</span>
                </div>
                <input
                  type="number"
                  min={1}
                  disabled={!hasWriteAccess}
                  value={configured ?? ''}
                  placeholder={cfg.index_lots > 0 ? `Auto (${cfg.index_lots})` : 'Skip (0)'}
                  onChange={(e) => setIndexLots(symbol, e.target.value)}
                  className={`w-full bg-surface-container-lowest border border-outline-variant rounded px-2 py-1 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all ${!hasWriteAccess ? 'opacity-60 cursor-not-allowed' : ''}`}
                />
                <span className="text-[10px] text-on-surface-variant font-mono-code">
                  {effectiveLots > 0 ? `= ${effectiveLots * lotSize} qty` : 'skipped'}
                </span>
              </div>
            );
          })}
        </div>
      </div>

      <CallRulesCard cfg={cfg} setCfg={setCfg} disabled={!hasWriteAccess} preview={preview} />

      {/* Save / lock action bar — sticky so Save is reachable on a phone
          without scrolling past seven screens of settings. */}
      <div className="sticky bottom-0 z-10 flex flex-col gap-2.5 sm:flex-row sm:items-center sm:justify-between sm:gap-4 bg-surface-container-lowest/95 backdrop-blur border border-outline-variant rounded-lg px-3 sm:px-4 py-2.5 sm:py-3 shadow-md">
        <span className={`text-xs ${saveError ? 'text-error font-semibold' : 'text-on-surface-variant hidden sm:block'}`}>
          {saveError ?? 'Changes apply immediately on save — including recomputing SL & next target on any open dynamic-targeting runner.'}
        </span>
        <div className="flex items-center gap-2 shrink-0">
          <button
            onClick={handleSave}
            disabled={saving || !hasWriteAccess}
            title={!hasWriteAccess ? 'Write access required to save changes' : undefined}
            className="flex-1 sm:flex-none flex items-center justify-center gap-1.5 px-4 py-2 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded-lg transition-colors font-body-bold shadow-sm"
          >
            <Save size={14} />
            {saving ? 'Saving…' : saved ? '✓ Saved' : 'Save Settings'}
          </button>
          {hasWriteAccess ? (
            <button
              type="button"
              onClick={lock}
              className="flex-1 sm:flex-none flex items-center justify-center gap-1.5 px-4 py-2 bg-error/10 hover:bg-error/20 text-error text-sm rounded-lg transition-colors font-body-bold shadow-sm"
              title="Lock session and switch to Read-Only mode"
            >
              <Lock size={14} />
              Lock Session
            </button>
          ) : (
            <button
              type="button"
              onClick={() => openUnlockModal('Sign in as the admin for write access')}
              className="flex-1 sm:flex-none flex items-center justify-center gap-1.5 px-4 py-2 bg-amber-500/10 hover:bg-amber-500/20 text-amber-400 text-sm rounded-lg transition-colors font-body-bold shadow-sm"
              title="Sign in as the admin for write access"
            >
              <KeyRound size={14} />
              Unlock
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
