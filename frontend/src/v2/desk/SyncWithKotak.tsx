import { useState } from 'react';
import { RefreshCw, X } from 'lucide-react';
import type { ReconcileAction, ReconcileActionKind, ReconcileApplyItem, ReconcileFinding } from '../../types';
import { reconcileApply, reconcilePreview } from '../lib/api';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';

// v1's "Sync with Kotak" (components/SyncWithKotakButton.tsx) in v2's style:
// the same preview, options, defaults and checks, sending the same requests.
// Never applies anything by itself — every mismatch is a question with
// explicit options, confirmed here before anything changes.

function keyOf(f: ReconcileFinding) {
  return f.position_id ?? `sym:${f.trading_symbol}`;
}

function kindOf(action: ReconcileAction): ReconcileActionKind {
  return typeof action === 'string' ? action : 'AdoptManual';
}

interface ManualInputs {
  stop_loss: string;
  target: string;
  /** Blank → the backend uses the broker's same-day average. */
  avg_buy_price: string;
}

const EMPTY_MANUAL: ManualInputs = { stop_loss: '', target: '', avg_buy_price: '' };

const FIELD = 'h-9 w-full rounded-6 border border-line bg-inset px-2.5 num-medium text-fg outline-none focus:border-line-strong';

export function SyncWithKotak({ serverBase, onSynced }: { serverBase: string; onSynced: () => void }) {
  const guard = useGuard();
  const [loading, setLoading] = useState(false);
  const [findings, setFindings] = useState<ReconcileFinding[] | null>(null);
  const [selections, setSelections] = useState<Record<string, ReconcileActionKind>>({});
  const [manualInputs, setManualInputs] = useState<Record<string, ManualInputs>>({});
  const [applying, setApplying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function openPreview() {
    setLoading(true);
    setError(null);
    const answer = await reconcilePreview(serverBase);
    setLoading(false);
    if ('error' in answer) {
      setError(answer.error);
      return;
    }
    setFindings(answer.findings);
    const initial: Record<string, ReconcileActionKind> = {};
    for (const f of answer.findings) {
      const rec = f.options.find((o) => o.recommended);
      if (rec) initial[keyOf(f)] = kindOf(rec.action);
    }
    setSelections(initial);
    setManualInputs({});
  }

  function selectManualEntry(key: string) {
    setSelections((s) => ({ ...s, [key]: 'AdoptManual' }));
    setManualInputs((m) => (m[key] ? m : { ...m, [key]: { ...EMPTY_MANUAL } }));
  }

  function updateManualInput(key: string, field: keyof ManualInputs, value: string) {
    setManualInputs((m) => ({ ...m, [key]: { ...(m[key] ?? EMPTY_MANUAL), [field]: value } }));
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
        const raw = manualInputs[keyOf(f)] ?? EMPTY_MANUAL;
        const stop_loss = parseFloat(raw.stop_loss);
        const target = parseFloat(raw.target);
        if (!Number.isFinite(stop_loss) || stop_loss <= 0 || !Number.isFinite(target) || target <= 0) {
          setError(`Enter a stop-loss and target greater than 0 for ${f.instrument} before applying.`);
          return;
        }
        // Optional: blank sends 0, and the backend falls back to the broker's
        // same-day average. A typed value must be a real price.
        let avg_buy_price = 0;
        if (raw.avg_buy_price.trim() !== '') {
          avg_buy_price = parseFloat(raw.avg_buy_price);
          if (!Number.isFinite(avg_buy_price) || avg_buy_price <= 0) {
            setError(`Buy price for ${f.instrument} must be greater than 0, or leave it blank to use the broker's average.`);
            return;
          }
        }
        items.push({ position_id: f.position_id, trading_symbol: f.trading_symbol, action: { AdoptManual: { stop_loss, target, avg_buy_price } } });
      } else {
        items.push({ position_id: f.position_id, trading_symbol: f.trading_symbol, action: kind });
      }
    }
    setApplying(true);
    const sent = await guard('Applying Sync with Kotak needs write access.', () => reconcileApply(serverBase, items), "Couldn't apply the sync");
    setApplying(false);
    if (!sent) return;
    if (!sent.ok) {
      setError(sent.error ?? 'Failed to apply changes');
      return;
    }
    setFindings(null);
    onSynced();
  }

  const actionable = findings?.filter((f) => f.options.length > 0) ?? [];
  const informational = findings?.filter((f) => f.options.length === 0) ?? [];

  return (
    <>
      <Button size="md" onClick={openPreview} disabled={loading} icon={<RefreshCw size={14} className={loading ? 'animate-spin' : ''} />}>
        {loading ? 'Checking Kotak…' : 'Sync with Kotak'}
      </Button>

      {error && !findings && (
        <Modal open onClose={() => setError(null)} label="Sync failed" width="lg:max-w-[420px]">
          <div className="flex flex-col gap-3 p-6">
            <h2 className="type-title text-loss">Sync failed</h2>
            <p className="type-body text-fg-muted">{error}</p>
            <Button size="md" onClick={() => setError(null)} className="self-end">
              Close
            </Button>
          </div>
        </Modal>
      )}

      {findings && (
        <Modal open onClose={() => setFindings(null)} label="Sync with Kotak" width="lg:max-w-[640px]" locked={applying}>
          <div className="flex items-start justify-between gap-4 border-b border-line px-6 py-4">
            <div>
              <h2 className="type-title text-fg">Sync with Kotak</h2>
              <p className="mt-0.5 type-caption text-fg-muted">Live comparison against the broker — nothing changes until you apply.</p>
            </div>
            <button type="button" aria-label="Close" onClick={() => setFindings(null)} disabled={applying} className="text-fg-muted hover:text-fg">
              <X size={18} />
            </button>
          </div>

          <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-6 py-4">
            {error && <div className="rounded-8 bg-loss-subtle px-3.5 py-2.5 type-caption text-loss">{error}</div>}
            {findings.length === 0 && <p className="py-6 text-center type-body text-fg-muted">Nothing tracked right now — nothing to compare.</p>}
            {actionable.map((f) => {
              const key = keyOf(f);
              const selectedKind = selections[key];
              return (
                <div key={key} className="rounded-12 bg-inset p-4">
                  <p className="mb-3 type-body text-fg">{f.message}</p>
                  <div className="flex flex-wrap gap-2">
                    {f.options.map((o) => {
                      const kind = kindOf(o.action);
                      const selected = selectedKind === kind;
                      return (
                        <button
                          key={kind}
                          type="button"
                          aria-pressed={selected}
                          onClick={() => (kind === 'AdoptManual' ? selectManualEntry(key) : setSelections((s) => ({ ...s, [key]: kind })))}
                          className={`h-8 rounded-6 px-3 type-button transition-colors ${selected ? 'bg-profit text-on-accent' : 'bg-raised text-fg-muted hover:text-fg'}`}
                        >
                          {o.label}
                          {o.recommended ? ' (recommended)' : ''}
                        </button>
                      );
                    })}
                  </div>

                  {selectedKind === 'AdoptManual' && (
                    <div className="mt-3 grid grid-cols-2 gap-3 border-t border-line pt-3 sm:grid-cols-3">
                      <label className="flex flex-col gap-1">
                        <span className="type-caption text-fg-muted">Stop loss (₹)</span>
                        <input type="number" min={0} step="0.05" value={manualInputs[key]?.stop_loss ?? ''} onChange={(e) => updateManualInput(key, 'stop_loss', e.target.value)} placeholder="e.g. 120.50" className={FIELD} />
                      </label>
                      <label className="flex flex-col gap-1">
                        <span className="type-caption text-fg-muted">Target 1 (₹)</span>
                        <input type="number" min={0} step="0.05" value={manualInputs[key]?.target ?? ''} onChange={(e) => updateManualInput(key, 'target', e.target.value)} placeholder="e.g. 180.00" className={FIELD} />
                      </label>
                      <label className="flex flex-col gap-1">
                        <span className="type-caption text-fg-muted">Buy price (₹)</span>
                        <input
                          type="number"
                          min={0}
                          step="0.05"
                          value={manualInputs[key]?.avg_buy_price ?? ''}
                          onChange={(e) => updateManualInput(key, 'avg_buy_price', e.target.value)}
                          placeholder={f.broker_avg_price > 0 ? `Broker avg ${f.broker_avg_price.toFixed(2)}` : 'e.g. 175.50'}
                          className={FIELD}
                        />
                      </label>
                      <p className="col-span-full type-caption text-fg-muted">
                        Buy price is this lot&apos;s actual entry — leave it blank to use the broker&apos;s
                        {f.broker_avg_price > 0 ? ` same-day average (₹${f.broker_avg_price.toFixed(2)})` : ' same-day average'}, which blends every fill of the symbol. If
                        dynamic targeting is on, the runner extends past target 1 like any other position — no target 2 needed.
                      </p>
                    </div>
                  )}
                </div>
              );
            })}

            {informational.length > 0 && (
              <div className="flex flex-col gap-1.5 border-t border-line pt-3">
                <div className="type-label text-fg-muted">No action available</div>
                {informational.map((f) => (
                  <p key={keyOf(f)} className="type-caption text-fg-muted">
                    {f.message}
                  </p>
                ))}
              </div>
            )}
          </div>

          <div className="flex justify-end gap-2 border-t border-line px-6 py-4">
            <Button size="md" onClick={() => setFindings(null)} disabled={applying}>
              Cancel
            </Button>
            <Button size="md" kind="profit" onClick={applySelections} disabled={applying || actionable.length === 0}>
              {applying ? 'Applying…' : 'Apply selected'}
            </Button>
          </div>
        </Modal>
      )}
    </>
  );
}
