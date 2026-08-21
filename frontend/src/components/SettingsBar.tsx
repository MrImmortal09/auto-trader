import { useEffect, useState } from 'react';
import { Save, Settings, LogOut } from 'lucide-react';
import type { TradingConfig } from '../types';
import { apiFetch } from '../lib/api';
import { clearToken } from '../lib/auth';
import { INDEX_LOT_REFERENCE } from '../lib/indexLots';

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
        // An older backend (mid-deploy skew) won't send this field at all —
        // default it so every read below can assume an object, not undefined.
        setCfg({ ...cfgData, index_lots_by_symbol: cfgData?.index_lots_by_symbol ?? {} });
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
      <div className="flex items-center gap-2 text-on-surface-variant text-sm px-4 py-3 font-medium">
        <Settings size={14} className="animate-spin text-primary" /> Loading settings…
      </div>
    );
  }

  function setIndexLots(symbol: string, raw: string) {
    setCfg((c) => {
      if (!c) return c;
      const next = { ...c.index_lots_by_symbol };
      if (raw === '') {
        delete next[symbol];
      } else {
        const n = parseInt(raw, 10);
        if (!Number.isNaN(n) && n > 0) next[symbol] = n;
      }
      return { ...c, index_lots_by_symbol: next };
    });
  }

  return (
    <>
    <div className="grid grid-cols-2 sm:flex sm:flex-wrap items-end gap-3 sm:gap-4 bg-surface border-b border-outline-variant px-4 sm:px-6 py-3.5">
      {/* Mode toggle */}
      <div className="flex flex-col gap-1 col-span-2 sm:col-span-1">
        <label className="text-label-caps text-on-surface-variant uppercase tracking-wide">
          Trading Mode
        </label>
        <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
          {(['PAPER', 'LIVE'] as const).map((m) => (
            <button
              key={m}
              onClick={() => setCfg((c) => c && { ...c, mode: m })}
              className={`flex-1 sm:flex-none px-3 py-1.5 transition-colors font-semibold ${
                cfg.mode === m
                  ? m === 'LIVE'
                    ? 'bg-error text-on-error font-bold'
                    : 'bg-secondary text-on-secondary font-bold'
                  : 'bg-surface-container-lowest text-on-surface-variant hover:bg-surface-container'
              }`}
            >
              {m}
            </button>
          ))}
        </div>
      </div>

      {/* Dynamic targeting toggle */}
      <div className="flex flex-col gap-1 col-span-2 sm:col-span-1">
        <label className="text-label-caps text-on-surface-variant uppercase tracking-wide">
          Dynamic Targeting
        </label>
        <div className="flex rounded-lg overflow-hidden border border-outline-variant text-xs font-label-caps">
          {([true, false] as const).map((on) => (
            <button
              key={String(on)}
              onClick={() => setCfg((c) => c && { ...c, dynamic_targeting: on })}
              title={on ? 'Sell one lot at target 1, then trail an extending target ladder for the runner' : 'Exit the runner at the signal\'s fixed target 2 (default)'}
              className={`flex-1 sm:flex-none px-3 py-1.5 transition-colors font-semibold ${
                cfg.dynamic_targeting === on
                  ? 'bg-secondary text-on-secondary font-bold'
                  : 'bg-surface-container-lowest text-on-surface-variant hover:bg-surface-container'
              }`}
            >
              {on ? 'ON' : 'OFF'}
            </button>
          ))}
        </div>
      </div>

      {/* Numeric inputs — the virtual (paper) wallet seed is meaningless once
          LIVE is selected, since LIVE reads the real broker balance instead. */}
      {fields
        .filter(({ key }) => key !== 'virtual_balance' || cfg.mode !== 'LIVE')
        .map(({ key, label }) => (
        <div key={key} className="flex flex-col gap-1">
          <label className="text-label-caps text-on-surface-variant uppercase tracking-wide truncate">
            {label}
          </label>
          <input
            type="number"
            value={key === 'virtual_balance' ? String(virtualBalance) : String(cfg[key])}
            onChange={(e) =>
              key === 'virtual_balance'
                ? setVirtualBalance(parseFloat(e.target.value) || 0)
                : setCfg((c) => c && { ...c, [key]: parseFloat(e.target.value) || 0 })
            }
            className="w-full sm:w-28 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1.5 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
          />
        </div>
      ))}

      {/* Save button */}
      <button
        onClick={handleSave}
        disabled={saving}
        className="col-span-2 sm:col-span-1 flex items-center justify-center gap-1.5 mt-2 sm:mt-5 px-4 py-2 sm:py-1.5 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded-lg transition-colors font-body-bold shadow-sm"
      >
        <Save size={14} />
        {saving ? 'Saving…' : saved ? '✓ Saved' : 'Save Settings'}
      </button>

      {/* Logout button */}
      <button
        onClick={() => {
          clearToken();
          if (typeof window !== 'undefined') window.location.reload();
        }}
        className="col-span-2 sm:col-span-1 flex items-center justify-center gap-1.5 mt-2 sm:mt-5 px-4 py-2 sm:py-1.5 bg-error/10 hover:bg-error/20 text-error text-sm rounded-lg transition-colors font-body-bold shadow-sm"
      >
        <LogOut size={14} />
        Logout
      </button>
    </div>

    {/* Per-index default lots — a trader who only trades indexes can size
        each one independently (e.g. 2 lots of SENSEX, 1 of MIDCPNIFTY)
        instead of one "Index Lots" number applied to all of them. */}
    <div className="bg-surface border-b border-outline-variant px-4 sm:px-6 py-3.5">
      <label className="text-label-caps text-on-surface-variant uppercase tracking-wide">
        Default Lots by Index
      </label>
      <div className="mt-2 grid grid-cols-2 sm:grid-cols-3 md:grid-cols-6 gap-2.5">
        {INDEX_LOT_REFERENCE.map(({ symbol, label, lotSize }) => {
          const configured = cfg.index_lots_by_symbol[symbol];
          const effectiveLots = configured && configured > 0 ? configured : cfg.index_lots;
          return (
            <div key={symbol} className="flex flex-col gap-1 bg-surface-container-lowest border border-outline-variant rounded-lg p-2.5">
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
                className="w-full bg-surface border border-outline-variant rounded px-2 py-1 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
              />
              <span className="text-[10px] text-on-surface-variant font-mono-code">
                = {effectiveLots * lotSize} qty
              </span>
            </div>
          );
        })}
      </div>
    </div>
    </>
  );
}
