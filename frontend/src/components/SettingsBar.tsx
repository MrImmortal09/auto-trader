import { useEffect, useState } from 'react';
import { Save, Settings, LogOut, Sliders, Wallet, Layers3 } from 'lucide-react';
import type { TradingConfig } from '../types';
import { apiFetch } from '../lib/api';
import { clearToken } from '../lib/auth';
import { INDEX_LOT_REFERENCE } from '../lib/indexLots';

const CARD = 'bg-surface-container-lowest border border-outline-variant rounded-xl p-4 sm:p-6 shadow-sm';
const CARD_HEADER = 'flex items-center gap-2 text-base font-bold text-on-surface mb-4';
const FIELD_LABEL = 'text-label-caps text-on-surface-variant uppercase tracking-wide truncate';
const NUMBER_INPUT = 'w-full bg-surface border border-outline-variant rounded px-2.5 py-1.5 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all';

function FactorSlider({ label, value, min, max, step, enabled, onChange, lowLabel, highLabel }: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  enabled: boolean;
  onChange: (v: number) => void;
  lowLabel: string;
  highLabel: string;
}) {
  return (
    <div className={`flex flex-col gap-1.5 sm:col-span-2 rounded-lg border border-outline-variant p-3.5 transition-opacity ${enabled ? 'bg-surface' : 'bg-surface opacity-50'}`}>
      <div className="flex items-center justify-between">
        <label className={FIELD_LABEL}>{label}</label>
        <span className="text-sm font-mono-code font-bold text-primary tabular-nums">
          {value.toFixed(2)}
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
          Only applies in LIVE mode when Dynamic Targeting is ON — and updates any already-open runner immediately on save.
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

  const fields: { key: keyof TradingConfig | 'virtual_balance'; label: string }[] = [
    { key: 'virtual_balance', label: 'Virtual Balance (₹)' },
    { key: 'index_lots', label: 'Index Lots' },
    { key: 'other_lots', label: 'Other Lots' },
    { key: 'brokerage_per_order', label: 'Brokerage (₹)' },
    { key: 'max_trade_amount_inr', label: 'Max Trade (₹)' },
    { key: 'target_1_exit_pct', label: 'Target 1 Exit %' },
    { key: 'target_2_exit_pct', label: 'Target 2 Exit %' },
    { key: 'entry_market_protection', label: 'Entry MP %' },
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
          <div className="flex flex-col gap-1.5">
            <label className={FIELD_LABEL}>Trading Mode</label>
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

          <div className="flex flex-col gap-1.5">
            <label className={FIELD_LABEL}>Dynamic Targeting</label>
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
            .map(({ key, label }) => (
              <div key={key} className="flex flex-col gap-1.5">
                <label className={FIELD_LABEL}>{label}</label>
                <input
                  type="number"
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
      </div>

      {/* Per-index default lots — a trader who only trades indexes can size
          each one independently (e.g. 2 lots of SENSEX, 1 of MIDCPNIFTY)
          instead of one "Index Lots" number applied to all of them. */}
      <div className={CARD}>
        <div className={CARD_HEADER}>
          <Layers3 size={16} className="text-primary" /> Default Lots by Index
        </div>
        <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-6 gap-2.5">
          {INDEX_LOT_REFERENCE.map(({ symbol, label, lotSize }) => {
            const configured = cfg.index_lots_by_symbol[symbol];
            // Mirror the backend's floor (lots_for_instrument always does
            // `.max(1)`) so this preview never shows a qty lower than what
            // will actually be bought.
            const effectiveLots = configured && configured > 0 ? configured : Math.max(1, cfg.index_lots);
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
                  placeholder={`Auto (${cfg.index_lots})`}
                  onChange={(e) => setIndexLots(symbol, e.target.value)}
                  className="w-full bg-surface-container-lowest border border-outline-variant rounded px-2 py-1 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
                />
                <span className="text-[10px] text-on-surface-variant font-mono-code">
                  = {effectiveLots * lotSize} qty
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
