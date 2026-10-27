import { useState } from 'react';
import { AlertOctagon, AlertTriangle, CheckCircle2, Loader2, X } from 'lucide-react';
import { apiFetch } from '../lib/api';
import { useAuth } from '../context/AuthContext';

interface KillSwitchModalProps {
  serverBase: string;
  isOpen: boolean;
  onClose: () => void;
  onSuccess: (active: boolean) => void;
  isAlreadyActive?: boolean;
}

export function KillSwitchModal({
  serverBase,
  isOpen,
  onClose,
  onSuccess,
  isAlreadyActive = false,
}: KillSwitchModalProps) {
  const { hasWriteAccess, openUnlockModal } = useAuth();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [resultMsg, setResultMsg] = useState<string | null>(null);

  if (!isOpen) return null;

  async function handleTriggerKillSwitch() {
    if (!hasWriteAccess) {
      openUnlockModal('Kill switch activation requires Write Access');
      return;
    }
    setLoading(true);
    setError(null);
    setResultMsg(null);
    try {
      const res = await apiFetch(serverBase, '/api/kill-switch', {
        method: 'POST',
      });
      if (!res.ok) {
        if (res.status === 401) {
          throw new Error('Session unauthorized or expired. Sign in again as the admin on the main page.');
        }
        const errData = await res.json().catch(() => null);
        throw new Error(errData?.error || `Request failed with status ${res.status}`);
      }
      const data = await res.json().catch(() => ({}));
      setResultMsg(
        `Kill switch engaged: Liquidated ${data.liquidated_positions ?? 0} position(s), cancelled ${data.abandoned_entries ?? 0} entry(ies).`
      );
      onSuccess(true);
      setTimeout(() => {
        onClose();
        setResultMsg(null);
      }, 1800);
    } catch (e: any) {
      setError(e.message || 'Error executing kill switch');
    } finally {
      setLoading(false);
    }
  }

  async function handleResetKillSwitch() {
    if (!hasWriteAccess) {
      openUnlockModal('Resetting kill switch requires Write Access');
      return;
    }
    setLoading(true);
    setError(null);
    setResultMsg(null);
    try {
      const res = await apiFetch(serverBase, '/api/kill-switch/reset', {
        method: 'POST',
      });
      if (!res.ok) {
        if (res.status === 401) {
          throw new Error('Session unauthorized or expired. Sign in again as the admin on the main page.');
        }
        const errData = await res.json().catch(() => null);
        throw new Error(errData?.error || `Request failed with status ${res.status}`);
      }
      setResultMsg('Kill switch disengaged. Normal trading resumed.');
      onSuccess(false);
      setTimeout(() => {
        onClose();
        setResultMsg(null);
      }, 1500);
    } catch (e: any) {
      setError(e.message || 'Error resetting kill switch');
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/60 backdrop-blur-sm animate-in fade-in duration-150">
      <div className="relative w-full max-w-md bg-surface-container-lowest border border-outline-variant rounded-2xl p-6 shadow-2xl space-y-5 text-on-surface">
        {/* Header */}
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2.5">
            <div className={`p-2 rounded-xl ${isAlreadyActive ? 'bg-amber-500/10 text-amber-500' : 'bg-red-500/10 text-red-500'}`}>
              <AlertOctagon size={24} />
            </div>
            <div>
              <h2 className="text-lg font-bold tracking-tight text-on-surface">
                {isAlreadyActive ? 'Kill Switch Active' : 'Emergency Kill Switch'}
              </h2>
              <p className="text-xs text-on-surface-variant">
                {isAlreadyActive ? 'Trading is currently halted' : 'Liquidate holdings & halt trading'}
              </p>
            </div>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded-lg text-on-surface-variant hover:text-on-surface hover:bg-surface-container-high transition-colors"
          >
            <X size={20} />
          </button>
        </div>

        {/* Read-Only Mode Notice */}
        {!hasWriteAccess && (
          <div className="p-3 rounded-xl bg-amber-500/10 border border-amber-500/20 text-xs text-amber-300 flex items-center justify-between">
            <span>Write access is required to engage or reset the kill switch.</span>
            <button
              type="button"
              onClick={() => openUnlockModal('Kill switch operations require Write Access')}
              className="ml-2 px-2.5 py-1 bg-amber-500/20 hover:bg-amber-500/30 text-amber-300 font-bold rounded-lg transition-colors shrink-0"
            >
              Unlock
            </button>
          </div>
        )}

        {/* Status / Warnings */}
        {!isAlreadyActive ? (
          <div className="space-y-3">
            <div className="p-3.5 rounded-xl bg-red-500/10 border border-red-500/20 text-xs text-red-400 flex items-start gap-2.5 leading-relaxed">
              <AlertTriangle size={18} className="shrink-0 mt-0.5 text-red-400" />
              <div>
                <p className="font-semibold text-red-300 mb-1">Critical Action Warning:</p>
                <ul className="list-disc list-inside space-y-1 text-[11px] text-red-400/90">
                  <li><strong>Halts Trading:</strong> Discards all new incoming signals and cancels waiting entries.</li>
                  <li><strong>Liquidates Holdings:</strong> Exits all open positions at market price immediately.</li>
                  <li><strong>Cancels Orders:</strong> In LIVE mode, cancels resting broker stops first to avoid overselling.</li>
                </ul>
              </div>
            </div>
            <p className="text-xs text-on-surface-variant text-center">
              Are you sure you want to proceed with this emergency liquidation?
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            <div className="p-3.5 rounded-xl bg-amber-500/10 border border-amber-500/20 text-xs text-amber-300 flex items-start gap-2.5 leading-relaxed">
              <AlertTriangle size={18} className="shrink-0 mt-0.5 text-amber-400" />
              <div>
                <p className="font-semibold mb-1">Trading is Halted for Today</p>
                <p className="text-[11px] text-amber-400/90">
                  The kill switch is currently engaged. All incoming trade signals are being blocked. You can disengage it below to resume automated trading.
                </p>
              </div>
            </div>
          </div>
        )}

        {/* Message / Feedback */}
        {error && (
          <div className="p-3 rounded-lg bg-red-500/10 border border-red-500/20 text-xs text-red-400 font-medium">
            {error}
          </div>
        )}
        {resultMsg && (
          <div className="p-3 rounded-lg bg-emerald-500/10 border border-emerald-500/20 text-xs text-emerald-400 font-medium flex items-center gap-2">
            <CheckCircle2 size={16} />
            <span>{resultMsg}</span>
          </div>
        )}

        {/* Action Buttons */}
        <div className="flex gap-3 pt-2">
          <button
            type="button"
            onClick={onClose}
            disabled={loading}
            className="flex-1 py-2.5 px-4 rounded-xl border border-outline-variant text-xs font-semibold text-on-surface hover:bg-surface-container-high transition-colors disabled:opacity-50"
          >
            Close
          </button>
          {!isAlreadyActive ? (
            <button
              type="button"
              onClick={handleTriggerKillSwitch}
              disabled={loading || !hasWriteAccess}
              className="flex-1 py-2.5 px-4 rounded-xl bg-red-600 hover:bg-red-500 text-white text-xs font-bold transition-colors flex items-center justify-center gap-2 shadow-lg shadow-red-900/20 disabled:opacity-50"
            >
              {loading ? (
                <>
                  <Loader2 size={15} className="animate-spin" />
                  <span>Liquidating…</span>
                </>
              ) : (
                <span>Confirm & Liquidate</span>
              )}
            </button>
          ) : (
            <button
              type="button"
              onClick={handleResetKillSwitch}
              disabled={loading || !hasWriteAccess}
              className="flex-1 py-2.5 px-4 rounded-xl bg-emerald-600 hover:bg-emerald-500 text-white text-xs font-bold transition-colors flex items-center justify-center gap-2 shadow-lg shadow-emerald-900/20 disabled:opacity-50"
            >
              {loading ? (
                <>
                  <Loader2 size={15} className="animate-spin" />
                  <span>Resuming…</span>
                </>
              ) : (
                <span>Resume Trading</span>
              )}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
