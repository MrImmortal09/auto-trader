import { useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronUp, Wifi, WifiOff } from 'lucide-react';
import { apiFetch, apiUrl } from '../lib/api';
import { getToken } from '../lib/auth';
import { useAuth } from '../context/AuthContext';

export function LogTerminal({ serverBase, height = 220 }: { serverBase: string; height?: number }) {
  const { hasWriteAccess, openUnlockModal } = useAuth();
  const [logs, setLogs] = useState<{ id: number, text: string, time: string, isError: boolean }[]>([]);
  const [filter, setFilter] = useState<'ALL' | 'ERROR'>('ALL');
  const [connected, setConnected] = useState(false);
  // Collapsed by default on phones: open, it takes ~220px of an ~840px screen
  // on every page (the Settings page was left with half the screen). One tap
  // on the header bar opens it.
  const [isOpen, setIsOpen] = useState(
    () => typeof window === 'undefined' || !window.matchMedia('(max-width: 767px)').matches
  );
  const bottomRef = useRef<HTMLDivElement>(null);

  // Auto-scroll on new log
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [logs]);

  // SSE connection & Initial load
  useEffect(() => {
    // 1. Fetch Historical Logs
    apiFetch(serverBase, '/api/logs/history')
      .then(res => res.json())
      .then((history: any[]) => {
        if (Array.isArray(history)) {
          const historicalLogs = history.map((item: any, i: number) => {
            const rawMsg = item.message;
            const text = typeof rawMsg === 'string' ? rawMsg : JSON.stringify(rawMsg);
            const isError = text.includes('"level":"ERROR"') || text.includes('"event":"ERROR"');

            // Format timestamp from "2026-07-20 06:21:23"
            let timeStr = 'Hist';
            if (item.timestamp) {
              // Backend stores exactly "YYYY-MM-DD HH:MM:SS" in IST. Parse it directly to ignore browser timezone.
              const timePart = item.timestamp.split(' ')[1];
              if (timePart) {
                const [h, m, s] = timePart.split(':');
                let hours = parseInt(h, 10);
                const ampm = hours >= 12 ? 'PM' : 'AM';
                hours = hours % 12;
                hours = hours ? hours : 12;
                timeStr = `${String(hours).padStart(2, '0')}:${m}:${s} ${ampm}`;
              }
            }
            return { id: Date.now() + i + Math.random(), text, time: timeStr, isError };
          });
          setLogs(historicalLogs);
        }
      })
      .catch(console.error);

    // 2. Connect SSE
    let es: EventSource | null = null;
    try {
      const token = getToken();
      const sseUrl = apiUrl(serverBase, '/api/logs/stream') + (token ? `?token=${encodeURIComponent(token)}` : '');
      es = new EventSource(sseUrl);
    } catch (e) {
      console.error(e);
      setConnected(false);
      return () => {};
    }
    es.onopen = () => setConnected(true);
    es.onmessage = (e: MessageEvent<string>) => {
      // Force current time to IST
      const nowIST = new Date(new Date().toLocaleString("en-US", { timeZone: "Asia/Kolkata" }));
      let hours = nowIST.getHours();
      const ampm = hours >= 12 ? 'PM' : 'AM';
      hours = hours % 12;
      hours = hours ? hours : 12; // the hour '0' should be '12'
      const timeStr = `${String(hours).padStart(2, '0')}:${String(nowIST.getMinutes()).padStart(2, '0')}:${String(nowIST.getSeconds()).padStart(2, '0')} ${ampm}`;

      const text = e.data;
      const isError = text.includes('"level":"ERROR"') || text.includes('"event":"ERROR"');

      setLogs((prev) => [...prev, { id: Date.now() + Math.random(), text, time: timeStr, isError }]);
    };
    es.onerror = () => setConnected(false);
    return () => es?.close();
  }, [serverBase]);

  return (
    <div className="flex flex-col border-t border-outline-variant bg-surface-container-lowest transition-all duration-300" style={{ height: isOpen ? height : 37 }}>
      {/* Header bar */}
      <div className="flex items-center gap-2 bg-surface-container-low px-3 sm:px-4 py-2 border-b border-outline-variant shrink-0 min-w-0">
        <button
          onClick={() => setIsOpen(!isOpen)}
          aria-label={isOpen ? 'Collapse engine log' : 'Expand engine log'}
          className="text-on-surface-variant hover:text-on-surface transition-colors p-0.5 rounded hover:bg-surface-container shrink-0"
        >
          {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
        </button>
        <div className={`w-2 h-2 rounded-full shrink-0 ${connected ? 'bg-secondary' : 'bg-error'}`} />
        {connected
          ? <Wifi size={13} className="text-secondary shrink-0" />
          : <WifiOff size={13} className="text-error shrink-0" />}
        <button
          type="button"
          onClick={() => setIsOpen(!isOpen)}
          className="min-w-0 truncate text-left text-xs text-on-surface-variant font-mono-code font-semibold"
        >
          <span className="sm:hidden">Engine Log</span>
          <span className="hidden sm:inline">Live Engine Log — /api/logs/stream</span>
        </button>
        <div className="ml-auto sm:ml-4 flex gap-1 bg-surface rounded p-0.5 border border-outline-variant shrink-0">
          <button
            onClick={() => setFilter('ALL')}
            className={`px-2 py-0.5 rounded text-[10px] uppercase font-bold transition-colors ${filter === 'ALL' ? 'bg-primary text-on-primary shadow-sm' : 'text-on-surface-variant hover:text-on-surface'}`}
          ><span className="sm:hidden">All</span><span className="hidden sm:inline">All Logs</span></button>
          <button
            onClick={() => setFilter('ERROR')}
            className={`px-2 py-0.5 rounded text-[10px] uppercase font-bold transition-colors ${filter === 'ERROR' ? 'bg-error text-on-error shadow-sm' : 'text-on-surface-variant hover:text-on-surface'}`}
          ><span className="sm:hidden">Errors</span><span className="hidden sm:inline">Error Logs</span></button>
        </div>
        <button
          onClick={() => setLogs([])}
          className="sm:ml-auto shrink-0 whitespace-nowrap text-xs text-on-surface-variant hover:text-on-surface transition-colors font-medium"
        >
          <span className="sm:hidden">Clear</span>
          <span className="hidden sm:inline">Clear UI Logs</span>
        </button>
        <button
          onClick={async () => {
            if (!hasWriteAccess) {
              openUnlockModal('Clearing the database requires Write Access');
              return;
            }
            if (confirm('Are you sure you want to clear the entire database (logs, trades, and positions)?')) {
              try {
                const res = await apiFetch(serverBase, '/api/settings/clear_database', { method: 'POST' });
                if (res.ok) setLogs([]);
                else alert('Failed to clear database');
              } catch (e) {
                console.error(e);
                alert('Failed to clear database');
              }
            }
          }}
          className="ml-1 sm:ml-4 shrink-0 whitespace-nowrap text-xs text-error hover:text-error/80 transition-colors font-semibold"
        >
          Clear DB
        </button>
      </div>

      {/* Scrollable body */}
      {isOpen && (
        <div className="flex-1 overflow-y-auto bg-surface-container-lowest px-4 py-2.5 font-mono-code text-xs leading-5 text-on-surface">
          {logs.length === 0 ? (
            <span className="text-on-surface-variant/70">Waiting for engine events…</span>
          ) : (
            logs.filter(log => filter === 'ALL' || log.isError).map((log) => {
              let display = log.text;
              let isTgMsg = false;
              try {
                const parsed = JSON.parse(log.text);
                if (parsed.event === 'TELEGRAM_MESSAGE') {
                  display = `[TG Message - Chat ${parsed.chat_id}]\n${parsed.text}`;
                  isTgMsg = true;
                } else {
                  display = JSON.stringify(parsed, null, 0);
                }
              } catch { /* raw */ }

              const colour =
                isTgMsg                         ? 'text-primary font-medium'
                : log.text.includes('ENTRY')          ? 'text-secondary font-semibold'
                : log.text.includes('SL_HIT') || log.text.includes('SL_TRAILED') ? 'text-error font-semibold'
                : log.text.includes('TGT')          ? 'text-tertiary font-semibold'
                : log.text.includes('CONFIG_UPDATED') ? 'text-surface-tint font-medium'
                : log.isError                      ? 'text-error font-semibold'
                : log.text.includes('"level":"WARN"')  ? 'text-tertiary font-medium'
                : 'text-on-surface';

              return (
                <div key={log.id} className={`${colour} flex whitespace-pre-wrap py-0.5 border-b border-outline-variant/30 last:border-0`}>
                  <span className="text-on-surface-variant select-none mr-2 shrink-0">[{log.time}]</span>
                  <span className="text-on-surface-variant/60 select-none mr-2 shrink-0">&gt;</span>
                  <span className="break-words">{display}</span>
                </div>
              );
            })
          )}
          <div ref={bottomRef} />
        </div>
      )}
    </div>
  );
}
