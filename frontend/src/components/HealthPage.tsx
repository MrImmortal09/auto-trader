import { useState } from 'react';
import { Activity, Cpu, HardDrive, Server, KeyRound, RefreshCw } from 'lucide-react';
import type { HealthSnapshot, KotakStatus } from '../types';
import { apiFetch } from '../lib/api';
import { fmtPct, fmtUptime } from '../lib/format';
import { Stat } from './Stat';

const CARD = 'bg-surface-container-lowest border border-outline-variant rounded-xl overflow-hidden shadow-sm';
const CARD_HEADER = 'flex items-center gap-2 px-4 sm:px-6 py-3 border-b border-outline-variant bg-surface-container-low text-sm font-bold text-on-surface';

export function HealthPage({ serverBase }: { serverBase: string }) {
  const [snapshot, setSnapshot] = useState<HealthSnapshot | null>(null);
  const [kotakStatus, setKotakStatus] = useState<KotakStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  async function loadHealth() {
    setLoading(true);
    setError('');
    try {
      const [healthRes, kotakRes] = await Promise.all([
        apiFetch(serverBase, '/api/health'),
        apiFetch(serverBase, '/api/auth/kotak'),
      ]);
      const data = await healthRes.json();
      if (!healthRes.ok) {
        setError(data?.error ?? 'Failed to load health snapshot');
        return;
      }
      setSnapshot(data);
      // Kotak status is a secondary diagnostic — don't fail the whole page
      // fetch if it errors, just leave the auto-login card off.
      setKotakStatus(kotakRes.ok ? await kotakRes.json() : null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="space-y-4">
      <div className={`${CARD} flex items-center justify-between gap-4 px-4 sm:px-6 py-4`}>
        <div>
          <div className="text-sm font-semibold text-on-surface">On-demand instance health</div>
          <div className="text-xs text-on-surface-variant">Fetches CPU, memory, swap, uptime, load average, and current server-process stats only when requested.</div>
        </div>
        <button
          onClick={loadHealth}
          disabled={loading}
          className="flex items-center gap-1.5 shrink-0 px-3.5 py-2 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded-lg transition-colors font-semibold shadow-sm"
        >
          <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
          {loading ? 'Refreshing…' : 'Fetch Health'}
        </button>
      </div>

      {!serverBase && (
        <div className="bg-amber-50 border border-amber-300 text-amber-800 rounded-lg px-4 py-3 text-sm">
          Set the backend server URL in the Kotak panel first if this frontend is running on a different origin than the backend.
        </div>
      )}

      {error && (
        <div className="bg-error-container border border-error text-on-error-container rounded-lg px-4 py-3 text-sm">
          {error}
        </div>
      )}

      {!snapshot && !error && !loading && (
        <div className={`${CARD} px-4 py-8 text-sm text-on-surface-variant text-center`}>
          No snapshot loaded yet — click "Fetch Health" above.
        </div>
      )}

      {snapshot && (
        <>
          {!snapshot.auth_secret_configured && (
            <div className="bg-error-container border border-error text-on-error-container rounded-lg px-4 py-3 text-sm">
              <span className="font-semibold">AUTH_SECRET is not set</span> on the backend — every authenticated
              API route will fail with 500 until it's configured and the server is restarted.
            </div>
          )}

          {kotakStatus && (
            <div className={CARD}>
              <div className={`${CARD_HEADER} justify-between`}>
                <span className="flex items-center gap-2">
                  <KeyRound size={15} className="text-primary" /> Kotak Auto-Login
                </span>
                <span className={`px-2 py-0.5 rounded text-[11px] font-label-caps font-bold uppercase tracking-wide ${
                  kotakStatus.auto_login_ready ? 'bg-[#d1fae5] text-[#065f46]' : 'bg-[#ffe4e6] text-[#9f1239]'
                }`}>
                  {kotakStatus.auto_login_ready ? 'ON' : 'OFF'}
                </span>
              </div>
              <div className="p-4 sm:p-6 grid gap-3 sm:grid-cols-2 text-sm">
                <div><span className="text-on-surface-variant">Session connected:</span> <span className="text-on-surface font-medium">{kotakStatus.connected ? 'Yes' : 'No'}</span></div>
                <div><span className="text-on-surface-variant">KOTAK_AUTO_LOGIN:</span> <span className="text-on-surface font-medium">{kotakStatus.auto_login_enabled ? 'enabled' : 'disabled'}</span></div>
                <div><span className="text-on-surface-variant">TOTP secret set:</span> <span className="text-on-surface font-medium">{kotakStatus.has_totp_secret ? 'Yes' : 'No'}</span></div>
                <div><span className="text-on-surface-variant">All KOTAK_* fields set:</span> <span className="text-on-surface font-medium">{kotakStatus.has_env_credentials ? 'Yes' : 'No'}</span></div>
              </div>
              {!kotakStatus.auto_login_ready && kotakStatus.auto_login_reason && (
                <div className="px-4 sm:px-6 pb-4 text-xs text-error">{kotakStatus.auto_login_reason}</div>
              )}
            </div>
          )}

          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
            <Stat icon={<Cpu size={16} className="text-primary" />} label="CPU Usage">
              {fmtPct(snapshot.cpu_usage_pct)}
            </Stat>
            <Stat icon={<Activity size={16} className="text-secondary" />} label="Load Average">
              {snapshot.load_average.one.toFixed(2)} / {snapshot.load_average.five.toFixed(2)} / {snapshot.load_average.fifteen.toFixed(2)}
            </Stat>
            <Stat icon={<HardDrive size={16} className="text-tertiary" />} label="Memory Used">
              {snapshot.memory.used_mib} / {snapshot.memory.total_mib} MiB
            </Stat>
            <Stat icon={<HardDrive size={16} className="text-surface-tint" />} label="Swap Used">
              {snapshot.swap.used_mib} / {snapshot.swap.total_mib} MiB
            </Stat>
          </div>

          <div className="grid gap-4 xl:grid-cols-[1.2fr_1fr]">
            <div className={CARD}>
              <div className={CARD_HEADER}>
                <Server size={15} className="text-primary" /> Instance Overview
              </div>
              <div className="p-4 sm:p-6 grid gap-3 sm:grid-cols-2 text-sm">
                <div><span className="text-on-surface-variant">Generated (IST):</span> <span className="text-on-surface font-medium">{snapshot.generated_at_ist}</span></div>
                <div><span className="text-on-surface-variant">Hostname:</span> <span className="text-on-surface font-medium">{snapshot.hostname ?? '—'}</span></div>
                <div><span className="text-on-surface-variant">OS:</span> <span className="text-on-surface font-medium">{[snapshot.os_name, snapshot.os_version].filter(Boolean).join(' ') || '—'}</span></div>
                <div><span className="text-on-surface-variant">Kernel:</span> <span className="text-on-surface font-medium">{snapshot.kernel_version ?? '—'}</span></div>
                <div><span className="text-on-surface-variant">Uptime:</span> <span className="text-on-surface font-medium">{fmtUptime(snapshot.uptime_secs)}</span></div>
                <div><span className="text-on-surface-variant">CPU Cores:</span> <span className="text-on-surface font-medium">{snapshot.cpu_cores}</span></div>
                <div><span className="text-on-surface-variant">Free Memory:</span> <span className="text-on-surface font-medium">{snapshot.memory.free_mib} MiB</span></div>
                <div><span className="text-on-surface-variant">Free Swap:</span> <span className="text-on-surface font-medium">{snapshot.swap.free_mib} MiB</span></div>
              </div>
            </div>

            <div className={CARD}>
              <div className={CARD_HEADER}>
                <Activity size={15} className="text-primary" /> Current Server Process
              </div>
              <div className="p-4 sm:p-6 text-sm space-y-2">
                {snapshot.current_process ? (
                  <>
                    <div><span className="text-on-surface-variant">PID:</span> <span className="text-on-surface font-medium">{snapshot.current_process.pid}</span></div>
                    <div><span className="text-on-surface-variant">Name:</span> <span className="text-on-surface font-medium">{snapshot.current_process.name}</span></div>
                    <div><span className="text-on-surface-variant">CPU:</span> <span className="text-on-surface font-medium">{fmtPct(snapshot.current_process.cpu_usage_pct)}</span></div>
                    <div><span className="text-on-surface-variant">Resident Memory:</span> <span className="text-on-surface font-medium">{snapshot.current_process.memory_mib} MiB</span></div>
                    <div><span className="text-on-surface-variant">Virtual Memory:</span> <span className="text-on-surface font-medium">{snapshot.current_process.virtual_memory_mib} MiB</span></div>
                    <div><span className="text-on-surface-variant">Run Time:</span> <span className="text-on-surface font-medium">{fmtUptime(snapshot.current_process.run_time_secs)}</span></div>
                  </>
                ) : (
                  <div className="text-on-surface-variant">Current process stats unavailable.</div>
                )}
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
