import { useEffect, useState } from 'react';
import { Save, Settings, LogOut, Sliders, Wallet, Layers3, Info } from 'lucide-react';
import type { TradingConfig } from '../types';
import { apiFetch } from '../lib/api';
import { clearToken } from '../lib/auth';
import { INDEX_LOT_REFERENCE } from '../lib/indexLots';

const CARD = 'bg-surface-container-lowest border border-outline-variant rounded-xl p-4 sm:p-6 shadow-sm';
const CARD_HEADER = 'flex items-center gap-2 text-base font-bold text-on-surface mb-4';
const FIELD_LABEL = 'text-label-caps text-on-surface-variant uppercase tracking-wide truncate';
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

function FactorSlider({ label, value, min, max, step, enabled, onChange, lowLabel, highLabel, decimals, disabledNote, info }: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  enabled: boolean;
  onChange: (v: number) => void;
  lowLabel: string;
  highLabel: string;
  decimals?: number;
  disabledNote?: string;
  info?: string;
}) {
  return (
    <div className={`relative flex flex-col gap-1.5 sm:col-span-2 rounded-lg border border-outline-variant p-3.5 transition-opacity ${enabled ? 'bg-surface' : 'bg-surface opacity-50'}`}>
      <div className="flex items-center justify-between">
        <span className="flex items-center gap-1 min-w-0">
          <label className={FIELD_LABEL}>{label}</label>
          {info && <InfoTip text={info} />}
        </span>
        <span className="text-sm font-mono-code font-bold text-primary tabular-nums">
          {value.toFixed(decimals ?? 2)}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={!enabled}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        className="w-full accent-primary disabled:cursor-not-allowed"
      />
      <div className="flex justify-between text-[10px] text-on-surface-variant font-mono-code">
        <span>{lowLabel}</span>
        <span>{highLabel}</span>
      </div>
      {!enabled && (
        <span className="text-[11px] text-on-surface-variant italic">
          {disabledNote ?? 'Only applies in LIVE mode when Dynamic Targeting is ON — and updates any already-open runner immediately on save.'}
        </span>
      )}
    </div>
  );
}

export function SettingsBar({ serverBase }: { serverBase: string }) {
  const [cfg, setCfg] = useState<TradingConfig | null>(null);
  const [virtualBalance, setVirtualBalance] = useState<number>(0);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

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
      info: 'Share of the position sold when target 1 hits, rounded up to whole lots. Ignored when Dynamic Targeting is ON — that always sells exactly one lot at target 1.' },
    { key: 'target_2_exit_pct', label: 'Target 2 Exit %',
      info: 'Reserved — the engine currently always exits the entire remaining position when target 2 hits, whatever this is set to.' },
    { key: 'entry_market_protection', label: 'Entry MP %',
      info: 'Kotak market-price protection on LIVE entry buys: the order can fill at most this % above the trigger price, so a spike can’t fill you arbitrarily worse. Protective exits always use 0.' },
  ];

  useEffect(() => {
    Promise.all([
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
          pre_t1_trailing: cfgData?.pre_t1_trailing ?? false,
          pre_t1_trail_arm_pct: cfgData?.pre_t1_trail_arm_pct ?? 60,
          pre_t1_trail_factor: cfgData?.pre_t1_trail_factor ?? 0.5,
        });
        setVirtualBalance(typeof walletData?.balance === 'number' ? walletData.balance : 0);
      })
      .catch(console.error);
  }, [serverBase]);

  async function handleSave() {
    if (!cfg) return;
    setSaving(true);
    try {
      await Promise.all([
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
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      console.error(e);
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
      {/* Trading mode + risk controls */}
      <div className={CARD}>
        <div className={CARD_HEADER}>
          <Sliders size={16} className="text-primary" /> Trading Mode &amp; Risk
        </div>

        <div className="grid gap-5 sm:grid-cols-2">
          <div className="relative flex flex-col gap-1.5">
            <span className="flex items-center gap-1 min-w-0">
              <label className={FIELD_LABEL}>Trading Mode</label>
              <InfoTip text="PAPER simulates entries and exits at the live market price against the virtual wallet — no real orders. LIVE places real orders on your Kotak Neo account with real money." />
            </span>
            <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
              {(['PAPER', 'LIVE'] as const).map((m) => (
                <button
                  key={m}
                  onClick={() => setCfg((c) => c && { ...c, mode: m })}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.mode === m
                      ? m === 'LIVE'
                        ? 'bg-error text-on-error font-bold'
                        : 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  }`}
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
                  onClick={() => setCfg((c) => c && { ...c, dynamic_targeting: on })}
                  title={on ? 'Sell one lot at target 1, then trail an extending target ladder for the runner' : "Exit the runner at the signal's fixed target 2 (default)"}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.dynamic_targeting === on
                      ? 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  }`}
                >
                  {on ? 'ON' : 'OFF'}
                </button>
              ))}
            </div>
          </div>

          <FactorSlider
            label="Trail Factor (stop = rung − diff × factor)"
            value={cfg.dynamic_targeting_trail_factor}
            min={0}
            max={1}
            step={0.05}
            enabled={cfg.dynamic_targeting}
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
                  onClick={() => setCfg((c) => c && { ...c, pre_t1_trailing: on })}
                  title={on
                    ? 'Before target 1: once price covers the arm % of the way there, trail the stop below the peak so a near-miss reversal exits in profit instead of riding back to the original SL'
                    : "Keep the signal's original SL untouched until target 1 hits (default)"}
                  className={`flex-1 px-3 py-1.5 transition-colors font-semibold ${
                    cfg.pre_t1_trailing === on
                      ? 'bg-secondary text-on-secondary font-bold'
                      : 'bg-surface text-on-surface-variant hover:bg-surface-container'
                  }`}
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
              <div key={key} className="relative flex flex-col gap-1.5">
                <span className="flex items-center gap-1 min-w-0">
                  <label className={FIELD_LABEL}>{label}</label>
                  <InfoTip text={info} />
                </span>
                <input
                  type="number"
                  min={0}
                  value={key === 'virtual_balance' ? String(virtualBalance) : String(cfg[key])}
                  onChange={(e) =>
                    key === 'virtual_balance'
                      ? setVirtualBalance(parseFloat(e.target.value) || 0)
                      : setCfg((c) => c && { ...c, [key]: parseFloat(e.target.value) || 0 })
                  }
                  className={NUMBER_INPUT}
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
                  value={configured ?? ''}
                  placeholder={cfg.index_lots > 0 ? `Auto (${cfg.index_lots})` : 'Skip (0)'}
                  onChange={(e) => setIndexLots(symbol, e.target.value)}
                  className="w-full bg-surface-container-lowest border border-outline-variant rounded px-2 py-1 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
                />
                <span className="text-[10px] text-on-surface-variant font-mono-code">
                  {effectiveLots > 0 ? `= ${effectiveLots * lotSize} qty` : 'skipped'}
                </span>
              </div>
            );
          })}
        </div>
      </div>

      {/* Save / logout action bar */}
      <div className="flex items-center justify-between gap-4 bg-surface-container-lowest border border-outline-variant rounded-lg px-4 py-3 shadow-sm">
        <span className="text-xs text-on-surface-variant">
          Changes apply immediately on save — including recomputing SL &amp; next target on any open dynamic-targeting runner.
        </span>
        <div className="flex items-center gap-2 shrink-0">
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center justify-center gap-1.5 px-4 py-2 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded-lg transition-colors font-body-bold shadow-sm"
          >
            <Save size={14} />
            {saving ? 'Saving…' : saved ? '✓ Saved' : 'Save Settings'}
          </button>
          <button
            onClick={() => {
              clearToken();
              if (typeof window !== 'undefined') window.location.reload();
            }}
            className="flex items-center justify-center gap-1.5 px-4 py-2 bg-error/10 hover:bg-error/20 text-error text-sm rounded-lg transition-colors font-body-bold shadow-sm"
          >
            <LogOut size={14} />
            Logout
          </button>
        </div>
      </div>
    </div>
  );
}
