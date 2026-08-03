import { useEffect, useState } from 'react';
import { Save, Settings } from 'lucide-react';
import type { TradingConfig } from '../types';
import { apiFetch } from '../lib/api';

export function SettingsBar({ serverBase }: { serverBase: string }) {
  const [cfg, setCfg] = useState<TradingConfig | null>(null);
  const [virtualBalance, setVirtualBalance] = useState<number>(0);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    Promise.all([
      apiFetch(serverBase, '/api/settings').then((r) => r.json()),
      apiFetch(serverBase, '/api/wallet/balance').then((r) => r.json()),
    ])
      .then(([cfgData, walletData]) => {
        setCfg(cfgData);
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

  return (
    <div className="flex flex-wrap items-end gap-4 bg-surface border-b border-outline-variant px-6 py-3.5">
      {/* Mode toggle */}
      <div className="flex flex-col gap-1">
        <span className="text-label-caps text-on-surface-variant uppercase tracking-wide">Mode</span>
        <button
          onClick={() =>
            setCfg((c) => c && { ...c, mode: c.mode === 'PAPER' ? 'LIVE' : 'PAPER' })
          }
          className={`px-3 py-1 rounded text-xs font-label-caps uppercase transition-colors shadow-sm ${
            cfg.mode === 'LIVE'
              ? 'bg-error hover:bg-error/90 text-on-error'
              : 'bg-secondary hover:bg-secondary/90 text-on-secondary'
          }`}
        >
          {cfg.mode}
        </button>
      </div>

      {/* Numeric inputs */}
      {(
        [
          { key: 'virtual_balance', label: 'Virtual Balance (₹)' },
          { key: 'index_lots', label: 'Index Lots' },
          { key: 'other_lots', label: 'Other Lots' },
          { key: 'brokerage_per_order', label: 'Brokerage (₹)' },
          { key: 'max_trade_amount_inr', label: 'Max Trade (₹)' },
          { key: 'target_1_exit_pct', label: 'Target 1 Exit %' },
          { key: 'target_2_exit_pct', label: 'Target 2 Exit %' },
          { key: 'entry_market_protection', label: 'Entry MP %' },
        ] as { key: keyof TradingConfig | 'virtual_balance'; label: string }[]
      ).map(({ key, label }) => (
        <div key={key} className="flex flex-col gap-1">
          <label className="text-label-caps text-on-surface-variant uppercase tracking-wide">
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
            className="w-28 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
          />
        </div>
      ))}

      {/* Save button */}
      <button
        onClick={handleSave}
        disabled={saving}
        className="flex items-center gap-1.5 mt-5 px-4 py-1.5 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded transition-colors font-body-bold shadow-sm"
      >
        <Save size={14} />
        {saving ? 'Saving…' : saved ? '✓ Saved' : 'Save'}
      </button>
    </div>
  );
}
