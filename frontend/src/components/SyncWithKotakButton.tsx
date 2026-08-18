import { useState } from 'react';
import { RefreshCw, X } from 'lucide-react';
import type { ReconcileActionKind, ReconcileApplyItem, ReconcileFinding } from '../types';
import { apiFetch } from '../lib/api';

function keyOf(f: ReconcileFinding) {
  return f.position_id ?? `sym:${f.trading_symbol}`;
}

/** "Sync with Kotak" — on-demand comparison against real broker positions.
 * Never applies anything by itself: every mismatch is a question with
 * explicit options, confirmed here before anything changes. */
export function SyncWithKotakButton({ serverBase, onSynced }: { serverBase: string; onSynced: () => void }) {
  const [loading, setLoading] = useState(false);
  const [findings, setFindings] = useState<ReconcileFinding[] | null>(null);
  const [selections, setSelections] = useState<Record<string, ReconcileActionKind>>({});
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
        if (rec) initial[keyOf(f)] = rec.action;
      }
      setSelections(initial);
    } catch (e) {
      console.error(e);
      setError('Network error contacting the server');
    } finally {
      setLoading(false);
    }
  }

  async function applySelections() {
    if (!findings) return;
    const items: ReconcileApplyItem[] = findings
      .filter((f) => f.options.length > 0 && selections[keyOf(f)])
      .map((f) => ({ position_id: f.position_id, trading_symbol: f.trading_symbol, action: selections[keyOf(f)] }));
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
              {findings.length === 0 && (
                <div className="text-sm text-on-surface-variant text-center py-6">Nothing tracked right now — nothing to compare.</div>
              )}
              {actionable.map((f) => (
                <div key={keyOf(f)} className="border border-outline-variant rounded-lg p-3.5 bg-surface">
                  <div className="text-sm text-on-surface mb-2.5">{f.message}</div>
                  <div className="flex flex-wrap gap-2">
                    {f.options.map((o) => (
                      <button
                        key={o.action}
                        onClick={() => setSelections((s) => ({ ...s, [keyOf(f)]: o.action }))}
                        className={`px-3 py-1.5 rounded-lg text-xs font-semibold transition-colors ${
                          selections[keyOf(f)] === o.action
                            ? 'bg-primary text-on-primary'
                            : 'bg-surface-container text-on-surface-variant hover:bg-surface-container-high'
                        }`}
                      >
                        {o.label}
                        {o.recommended ? ' (recommended)' : ''}
                      </button>
                    ))}
                  </div>
                </div>
              ))}

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
