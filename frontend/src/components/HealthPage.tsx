import { useState } from 'react';
import { Activity, Cpu, HardDrive } from 'lucide-react';
import type { HealthSnapshot } from '../types';
import { apiFetch } from '../lib/api';
import { fmtPct, fmtUptime } from '../lib/format';
import { Stat } from './Stat';

export function HealthPage({ serverBase }: { serverBase: string }) {
  const [snapshot, setSnapshot] = useState<HealthSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  async function loadHealth() {
    setLoading(true);
    setError('');
    try {
      const res = await apiFetch(serverBase, '/api/health');
      const data = await res.json();
      if (!res.ok) {
        setError(data?.error ?? 'Failed to load health snapshot');
        return;
      }
      setSnapshot(data);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="flex-1 overflow-y-auto p-4 space-y-4 bg-background">
      <div className="flex items-center justify-between gap-4 bg-surface-container-lowest border border-outline-variant rounded-lg px-4 py-3 shadow-sm">
        <div>
          <div className="text-sm font-semibold text-on-surface">On-demand instance health</div>
          <div className="text-xs text-on-surface-variant">Fetches CPU, memory, swap, uptime, load average, and current server-process stats only when requested.</div>
        </div>
        <button
          onClick={loadHealth}
          disabled={loading}
          className="px-3 py-1.5 bg-primary-container hover:bg-primary disabled:opacity-50 text-on-primary text-sm rounded transition-colors font-medium shadow-sm"
        >
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
        <div className="bg-surface-container-lowest border border-outline-variant rounded-lg px-4 py-6 text-sm text-on-surface-variant">
          No snapshot loaded yet.
        </div>
      )}

      {snapshot && (
        <>
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
            <div className="bg-surface-container-lowest border border-outline-variant rounded-lg overflow-hidden shadow-sm">
              <div className="px-4 py-2 border-b border-outline-variant text-label-caps text-on-surface-variant uppercase tracking-wide bg-surface-container-low font-semibold">
                Instance Overview
              </div>
              <div className="p-4 grid gap-3 sm:grid-cols-2 text-sm">
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

            <div className="bg-surface-container-lowest border border-outline-variant rounded-lg overflow-hidden shadow-sm">
              <div className="px-4 py-2 border-b border-outline-variant text-label-caps text-on-surface-variant uppercase tracking-wide bg-surface-container-low font-semibold">
                Current Server Process
              </div>
              <div className="p-4 text-sm space-y-2">
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
