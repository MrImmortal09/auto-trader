import { useEffect, useState } from 'react';
import { apiFetch } from '../lib/api';
import { KotakLoginPanel } from './KotakLoginPanel';
import { TelegramLoginPanel } from './TelegramLoginPanel';

type SysStatus = {
  telegram_connected: boolean;
  kotak_connected: boolean;
  /** "YYYY-MM-DD HH:MM:SS" in IST, or null when no Scrip Master download has
   *  succeeded since the server started. Older backends omit the field. */
  scrip_loaded_at: string | null;
};

export function ConnectionPanel({ serverBase, onServerBaseChange }: {
  serverBase: string;
  onServerBaseChange: (value: string) => void;
}) {
  const [sysStatus, setSysStatus] = useState<SysStatus>({
    telegram_connected: false,
    kotak_connected: false,
    scrip_loaded_at: null,
  });
  const [isUpdating, setIsUpdating] = useState(false);

  useEffect(() => {
    if (!serverBase) return;
    const fetchStatus = async () => {
      try {
        const res = await apiFetch(serverBase, '/api/status');
        if (res.ok) setSysStatus(await res.json());
      } catch (e) {}
    };
    fetchStatus();
    const timer = setInterval(fetchStatus, 3000);
    return () => clearInterval(timer);
  }, [serverBase]);

  const handleDisconnectKotak = async () => {
    if (!confirm('Disconnect Kotak? The WebSocket will stop and the session will be cleared. Your input fields will not be touched.')) return;
    try {
      await apiFetch(serverBase, '/api/auth/kotak/disconnect', { method: 'DELETE' });
    } catch (e) {}
  };

  const handleDisconnectTelegram = async () => {
    if (!confirm('Disconnect Telegram? The ingester will stop and the session file will be deleted. Your input fields will not be touched.')) return;
    try {
      await apiFetch(serverBase, '/api/auth/telegram/disconnect', { method: 'DELETE' });
    } catch (e) {}
  };

  const handleReset = async () => {
    if (!confirm('Are you sure you want to reset all connections? This will log out Telegram and Kotak and restart the server!')) return;
    try {
      await apiFetch(serverBase, '/api/auth/reset', { method: 'DELETE' });
      alert('Connections reset. The server is restarting...');
      window.location.reload();
    } catch (e) {
      alert('Failed to reset connections');
    }
  };

  const handleUpdate = async () => {
    if (!confirm('Are you sure you want to update the server? This will download the latest release, disconnect everything, and restart the server.')) return;
    setIsUpdating(true);
    // The server kills itself (via tmux C-c) before it can flush the HTTP
    // response, so fetch() almost always rejects with a network error. That
    // is NOT a failure — it means the update script started successfully.
    // Only treat an explicit HTTP 500 body as a real error.
    let explicitError: string | null = null;
    try {
      const res = await apiFetch(serverBase, '/api/update_server', { method: 'POST' });
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        explicitError = body?.error ?? `HTTP ${res.status}`;
      }
    } catch {
      // Network error = server died mid-response = update is running. Fall through.
    }

    if (explicitError) {
      alert(`Failed to trigger update: ${explicitError}`);
      setIsUpdating(false);
      return;
    }

    // Poll /api/health until the new server comes back up, then reload.
    const pollTimer = setInterval(async () => {
      try {
        const res = await apiFetch(serverBase, '/api/health');
        if (res.ok) {
          clearInterval(pollTimer);
          window.location.reload();
        }
      } catch (e) {}
    }, 2000);
  };

  // The backend formats this in IST, so compare against today *in IST* rather
  // than the viewer's local date — the dashboard can be open from any timezone
  // but the trading day is always IST.
  const scripLoadedAt = sysStatus.scrip_loaded_at ?? null;
  const istToday = new Date().toLocaleDateString('en-CA', { timeZone: 'Asia/Kolkata' });
  const scripFreshToday = !!scripLoadedAt && scripLoadedAt.slice(0, 10) === istToday;
  const scripLabel = !scripLoadedAt
    ? 'never'
    : scripFreshToday
      ? scripLoadedAt.slice(11, 16)
      : scripLoadedAt.slice(0, 10);
  const scripDot = !scripLoadedAt
    ? 'bg-error'
    : scripFreshToday
      ? 'bg-secondary shadow-[0_0_5px_rgba(0,108,73,0.3)]'
      : 'bg-tertiary';
  const scripTitle = !scripLoadedAt
    ? 'Scrip Master has not been downloaded successfully since this server started — signals cannot be resolved to contracts'
    : `Scrip Master last downloaded ${scripLoadedAt} IST${scripFreshToday ? '' : ' — stale, nothing downloaded today'}`;

  return (
    <div className="flex flex-col gap-4 bg-surface-container-lowest border border-outline-variant rounded-xl px-4 sm:px-6 py-4 sm:py-5 shadow-sm">
      <div className="flex flex-wrap sm:flex-nowrap items-center justify-between gap-3 pb-4 border-b border-outline-variant">
        <div className="flex gap-4 text-xs font-semibold">
          {/* Kotak status + disconnect */}
          <div className="flex items-center gap-1.5">
            <div className={`w-2 h-2 rounded-full ${sysStatus.kotak_connected ? 'bg-secondary shadow-[0_0_5px_rgba(0,108,73,0.3)]' : 'bg-error'}`} />
            <span className={sysStatus.kotak_connected ? 'text-on-surface' : 'text-on-surface-variant'}>Kotak</span>
            {sysStatus.kotak_connected && (
              <button
                id="btn-disconnect-kotak"
                onClick={handleDisconnectKotak}
                className="ml-1 px-1.5 py-0.5 text-[10px] rounded bg-error-container hover:bg-error text-on-error-container hover:text-on-error border border-error/50 transition-colors font-medium"
              >
                Disconnect
              </button>
            )}
          </div>
          {/* Telegram status + disconnect */}
          <div className="flex items-center gap-1.5">
            <div className={`w-2 h-2 rounded-full ${sysStatus.telegram_connected ? 'bg-secondary shadow-[0_0_5px_rgba(0,108,73,0.3)]' : 'bg-error'}`} />
            <span className={sysStatus.telegram_connected ? 'text-on-surface' : 'text-on-surface-variant'}>Telegram</span>
            {sysStatus.telegram_connected && (
              <button
                id="btn-disconnect-telegram"
                onClick={handleDisconnectTelegram}
                className="ml-1 px-1.5 py-0.5 text-[10px] rounded bg-error-container hover:bg-error text-on-error-container hover:text-on-error border border-error/50 transition-colors font-medium"
              >
                Disconnect
              </button>
            )}
          </div>
          {/* Scrip Master freshness. The 09:10 session recycle only re-downloads
              when this is stale, so it also serves as proof that pass ran. */}
          <div className="flex items-center gap-1.5" title={scripTitle}>
            <div className={`w-2 h-2 rounded-full ${scripDot}`} />
            <span className={scripFreshToday ? 'text-on-surface' : 'text-on-surface-variant'}>Scrip</span>
            <span className="font-normal text-on-surface-variant">{scripLabel}</span>
          </div>
        </div>
        <div className="flex gap-2 w-full sm:w-auto justify-end">
          {/* Update Server */}
          <button
            onClick={handleUpdate}
            disabled={isUpdating}
            className={`btn-sm transition-colors shadow-sm ${isUpdating ? 'bg-primary/50 text-on-primary cursor-wait' : 'bg-primary hover:bg-primary/90 text-on-primary font-medium'}`}
            title="Download latest binary and restart"
          >
            {isUpdating ? 'Updating...' : 'Update Server'}
          </button>

          {/* Hard reset (restarts server) — kept for emergencies */}
          <button
            id="btn-reset-all-connections"
            onClick={handleReset}
            disabled={isUpdating}
            className="btn-sm bg-error-container hover:bg-error text-on-error-container hover:text-on-error border border-error/30 transition-colors disabled:opacity-50 font-medium"
            title="Full reset — restarts the server process"
          >
            Reset All
          </button>
        </div>
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        <div className="bg-surface border border-outline-variant rounded-lg p-3.5">
          <KotakLoginPanel serverBase={serverBase} onServerBaseChange={onServerBaseChange} />
        </div>
        <div className="bg-surface border border-outline-variant rounded-lg p-3.5">
          <TelegramLoginPanel serverBase={serverBase} />
        </div>
      </div>
    </div>
  );
}
