import { useState } from 'react';
import { RefreshCw, X } from 'lucide-react';
import type { ReconcileAction, ReconcileActionKind, ReconcileApplyItem, ReconcileFinding } from '../types';
import { apiFetch } from '../lib/api';

function keyOf(f: ReconcileFinding) {
  return f.position_id ?? `sym:${f.trading_symbol}`;
}

function kindOf(action: ReconcileAction): ReconcileActionKind {
  return typeof action === 'string' ? action : 'AdoptManual';
}

interface ManualInputs {
  stop_loss: string;
  target: string;
}

/** "Sync with Kotak" — on-demand comparison against real broker positions.
 * Never applies anything by itself: every mismatch is a question with
 * explicit options, confirmed here before anything changes. */
export function SyncWithKotakButton({ serverBase, onSynced }: { serverBase: string; onSynced: () => void }) {
  const [loading, setLoading] = useState(false);
  const [findings, setFindings] = useState<ReconcileFinding[] | null>(null);
  const [selections, setSelections] = useState<Record<string, ReconcileActionKind>>({});
  const [manualInputs, setManualInputs] = useState<Record<string, ManualInputs>>({});
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function openPreview() {
    setLoading(true);
    setError(null);
    try {
      const res = await apiFetch(serverBase, '/api/positions/reconcile/preview');
      const data = await res.json().catch(() => null);
      if (!res.ok) {
        setError(data?.error ?? 'Failed to fetch live data from Kotak');
        return;
      }
      const list: ReconcileFinding[] = Array.isArray(data) ? data : [];
      setFindings(list);
      const initial: Record<string, ReconcileActionKind> = {};
      for (const f of list) {
        const rec = f.options.find((o) => o.recommended);
        if (rec) initial[keyOf(f)] = kindOf(rec.action);
      }
      setSelections(initial);
      setManualInputs({});
    } catch (e) {
      console.error(e);
      setError('Network error contacting the server');
    } finally {
      setLoading(false);
    }
  }

  function selectManualEntry(key: string) {
    setSelections((s) => ({ ...s, [key]: 'AdoptManual' }));
    setManualInputs((m) => (m[key] ? m : { ...m, [key]: { stop_loss: '', target: '' } }));
  }

  function updateManualInput(key: string, field: keyof ManualInputs, value: string) {
    setManualInputs((m) => ({ ...m, [key]: { ...(m[key] ?? { stop_loss: '', target: '' }), [field]: value } }));
  }

  async function applySelections() {
    if (!findings) return;
    setError(null);

    const items: ReconcileApplyItem[] = [];
    for (const f of findings) {
      if (f.options.length === 0) continue;
      const kind = selections[keyOf(f)];
      if (!kind) continue;

      if (kind === 'AdoptManual') {
        const raw = manualInputs[keyOf(f)] ?? { stop_loss: '', target: '' };
        const stop_loss = parseFloat(raw.stop_loss);
        const target = parseFloat(raw.target);
        if (!Number.isFinite(stop_loss) || stop_loss <= 0 || !Number.isFinite(target) || target <= 0) {
          setError(`Enter a stop-loss and target greater than 0 for ${f.instrument} before applying.`);
          return;
        }
        items.push({ position_id: f.position_id, trading_symbol: f.trading_symbol, action: { AdoptManual: { stop_loss, target } } });
      } else {
        items.push({ position_id: f.position_id, trading_symbol: f.trading_symbol, action: kind });
      }
    }

    setApplying(true);
    try {
      const res = await apiFetch(serverBase, '/api/positions/reconcile/apply', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(items),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => null);
        setError(err?.error ?? 'Failed to apply changes');
        return;
      }
      setFindings(null);
      onSynced();
    } catch (e) {
      console.error(e);
      setError('Network error applying changes');
    } finally {
      setApplying(false);
    }
  }

  const actionable = findings?.filter((f) => f.options.length > 0) ?? [];
  const informational = findings?.filter((f) => f.options.length === 0) ?? [];

  return (
    <>
      <button
        onClick={openPreview}
        disabled={loading}
        className="flex items-center gap-1.5 px-3 py-1.5 bg-primary-container hover:bg-primary text-on-primary-container hover:text-on-primary rounded-lg text-xs font-semibold transition-colors disabled:opacity-50"
      >
        <RefreshCw size={13} className={loading ? 'animate-spin' : ''} />
        {loading ? 'Checking Kotak…' : 'Sync with Kotak'}
      </button>

      {error && !findings && (
        <div className="fixed inset-0 z-[200] flex items-center justify-center bg-inverse-surface/60 p-4" onClick={() => setError(null)}>
          <div className="bg-surface-container-lowest border border-error rounded-lg shadow-2xl max-w-md w-full p-5" onClick={(e) => e.stopPropagation()}>
            <div className="text-error font-bold mb-2">Sync failed</div>
            <div className="text-sm text-on-surface-variant mb-4">{error}</div>
            <button onClick={() => setError(null)} className="w-full py-2 bg-surface-container hover:bg-surface-container-high rounded-lg text-sm font-semibold text-on-surface transition-colors">
              Close
            </button>
          </div>
        </div>
      )}

      {findings && (
        <div className="fixed inset-0 z-[200] flex items-center justify-center bg-inverse-surface/60 p-4">
          <div className="bg-surface-container-lowest border border-outline-variant rounded-xl shadow-2xl max-w-2xl w-full max-h-[85vh] flex flex-col">
            <div className="px-5 py-4 border-b border-outline-variant flex items-center justify-between shrink-0">
              <div>
                <h3 className="font-bold text-on-surface">Sync with Kotak</h3>
                <p className="text-xs text-on-surface-variant mt-0.5">Live comparison against the broker — nothing changes until you apply.</p>
              </div>
              <button onClick={() => setFindings(null)} className="text-on-surface-variant hover:text-on-surface">
                <X size={18} />
              </button>
            </div>

            <div className="flex-1 overflow-y-auto p-5 space-y-3">
              {error && (
                <div className="bg-error-container border border-error text-on-error-container rounded-lg px-3.5 py-2.5 text-xs">
                  {error}
                </div>
              )}
              {findings.length === 0 && (
                <div className="text-sm text-on-surface-variant text-center py-6">Nothing tracked right now — nothing to compare.</div>
              )}
              {actionable.map((f) => {
                const key = keyOf(f);
                const selectedKind = selections[key];
                return (
                  <div key={key} className="border border-outline-variant rounded-lg p-3.5 bg-surface">
                    <div className="text-sm text-on-surface mb-2.5">{f.message}</div>
                    <div className="flex flex-wrap gap-2">
                      {f.options.map((o) => {
                        const kind = kindOf(o.action);
                        return (
                          <button
                            key={kind}
                            onClick={() => (kind === 'AdoptManual' ? selectManualEntry(key) : setSelections((s) => ({ ...s, [key]: kind })))}
                            className={`px-3 py-1.5 rounded-lg text-xs font-semibold transition-colors ${
                              selectedKind === kind
                                ? 'bg-primary text-on-primary'
                                : 'bg-surface-container text-on-surface-variant hover:bg-surface-container-high'
                            }`}
                          >
                            {o.label}
                            {o.recommended ? ' (recommended)' : ''}
                          </button>
                        );
                      })}
                    </div>

                    {selectedKind === 'AdoptManual' && (
                      <div className="mt-3 pt-3 border-t border-outline-variant/60 grid grid-cols-2 gap-3">
                        <div className="flex flex-col gap-1">
                          <label className="text-[10px] uppercase tracking-wide text-on-surface-variant font-semibold">Stop Loss (₹)</label>
                          <input
                            type="number"
                            min={0}
                            step="0.05"
                            value={manualInputs[key]?.stop_loss ?? ''}
                            onChange={(e) => updateManualInput(key, 'stop_loss', e.target.value)}
                            placeholder="e.g. 120.50"
                            className="bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1.5 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <label className="text-[10px] uppercase tracking-wide text-on-surface-variant font-semibold">Target 1 (₹)</label>
                          <input
                            type="number"
                            min={0}
                            step="0.05"
                            value={manualInputs[key]?.target ?? ''}
                            onChange={(e) => updateManualInput(key, 'target', e.target.value)}
                            placeholder="e.g. 180.00"
                            className="bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1.5 text-sm text-on-surface tabular-nums focus:outline-none focus:ring-2 focus:ring-primary focus:border-transparent transition-all"
                          />
                        </div>
                        <p className="col-span-2 text-[10px] text-on-surface-variant">
                          If Dynamic Targeting is on in Settings, the runner extends past this target the same as any other position — no separate target 2 needed.
                        </p>
                      </div>
                    )}
                  </div>
                );
              })}

              {informational.length > 0 && (
                <div className="pt-2 border-t border-outline-variant/50 space-y-1.5">
                  <div className="text-[10px] uppercase tracking-wider text-on-surface-variant font-semibold mb-1.5">No action available</div>
                  {informational.map((f) => (
                    <div key={keyOf(f)} className="text-xs text-on-surface-variant">{f.message}</div>
                  ))}
                </div>
              )}
            </div>

            <div className="px-5 py-4 border-t border-outline-variant flex items-center justify-end gap-2 shrink-0">
              <button
                onClick={() => setFindings(null)}
                className="px-4 py-2 rounded-lg text-sm font-semibold bg-surface-container hover:bg-surface-container-high text-on-surface transition-colors"
              >
                Cancel
              </button>
              <button
                onClick={applySelections}
                disabled={applying || actionable.length === 0}
                className="px-4 py-2 rounded-lg text-sm font-semibold bg-primary-container hover:bg-primary text-on-primary-container hover:text-on-primary disabled:opacity-50 transition-colors"
              >
                {applying ? 'Applying…' : 'Apply Selected'}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
