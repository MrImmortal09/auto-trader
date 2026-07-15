import { useEffect, useRef, useState } from 'react';
import {
  Activity,
  ChevronDown,
  ChevronUp,
  Cpu,
  HardDrive,
  IndianRupee,
  Info,
  KeyRound,
  MessageCircle,
  Plug,
  Save,
  Settings,
  TrendingUp,
  Wallet,
  Wifi,
  WifiOff,
} from 'lucide-react';

// ---------------------------------------------------------------------------
// Types (mirror shared_domain structs)
// ---------------------------------------------------------------------------

interface TradingConfig {
  max_trade_amount_inr: number;
  default_option_lots: number;
  mode: string;
  brokerage_per_order: number;
  target_1_exit_pct: number;
  target_2_exit_pct: number;
}

interface PaperTrade {
  id: number;
  ticker: string;
  action: string;
  qty: number;
  executed_price: number;
  gross_value: number;
  brokerage: number;
  stt_charge: number;
  sebi_fee: number;
  stamp_duty: number;
  transaction_charge: number;
  gst: number;
  net_value: number;
  timestamp: string;
}

interface Portfolio {
  balance: number;
  trades: PaperTrade[];
}

interface HealthSnapshot {
  generated_at_ist: string;
  hostname: string | null;
  os_name: string | null;
  os_version: string | null;
  kernel_version: string | null;
  uptime_secs: number;
  cpu_cores: number;
  cpu_usage_pct: number;
  load_average: {
    one: number;
    five: number;
    fifteen: number;
  };
  memory: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  swap: {
    total_mib: number;
    used_mib: number;
    free_mib: number;
  };
  current_process: {
    pid: string;
    name: string;
    cpu_usage_pct: number;
    memory_mib: number;
    virtual_memory_mib: number;
    run_time_secs: number;
  } | null;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function fmt(n: number) {
  return new Intl.NumberFormat('en-IN', {
    maximumFractionDigits: 2,
    minimumFractionDigits: 2,
  }).format(n);
}

function totalCharges(t: PaperTrade) {
  return t.brokerage + t.stt_charge + t.sebi_fee +
    t.stamp_duty + t.transaction_charge + t.gst;
}

function fmtPct(n: number) {
  return `${n.toFixed(1)}%`;
}

function fmtUptime(totalSecs: number) {
  const days = Math.floor(totalSecs / 86_400);
  const hours = Math.floor((totalSecs % 86_400) / 3_600);
  const mins = Math.floor((totalSecs % 3_600) / 60);
  if (days > 0) return `${days}d ${hours}h ${mins}m`;
  if (hours > 0) return `${hours}h ${mins}m`;
  return `${mins}m`;
}

const SERVER_BASE_STORAGE_KEY = 'server_base';
const SERVER_BASE_COOKIE = 'server_base';
const DEFAULT_SERVER_BASE = 'https://at.axiosiiitl.dev';

function readCookie(name: string) {
  if (typeof document === 'undefined') return '';
  const prefix = `${name}=`;
  const entry = document.cookie.split('; ').find((item) => item.startsWith(prefix));
  return entry ? decodeURIComponent(entry.slice(prefix.length)) : '';
}

function normalizeServerBase(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return '';
  return trimmed.replace(/\/+$/, '');
}

function isValidServerBase(value: string) {
  const normalized = normalizeServerBase(value);
  if (!normalized) return true;

  try {
    const parsed = new URL(normalized);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:';
  } catch {
    return false;
  }
}

function getStoredServerBase() {
  if (typeof window === 'undefined') return '';
  const saved = window.localStorage.getItem(SERVER_BASE_STORAGE_KEY) ?? '';
  const cookie = readCookie(SERVER_BASE_COOKIE);
  return normalizeServerBase(saved || cookie || (import.meta.env.VITE_API_BASE_URL ?? DEFAULT_SERVER_BASE));
}

function persistServerBase(value: string) {
  const normalized = normalizeServerBase(value);

  if (typeof window !== 'undefined') {
    if (normalized) window.localStorage.setItem(SERVER_BASE_STORAGE_KEY, normalized);
    else window.localStorage.removeItem(SERVER_BASE_STORAGE_KEY);
  }

  if (typeof document !== 'undefined') {
    document.cookie = normalized
      ? `${SERVER_BASE_COOKIE}=${encodeURIComponent(normalized)}; path=/; max-age=31536000; SameSite=Lax`
      : `${SERVER_BASE_COOKIE}=; path=/; max-age=0; SameSite=Lax`;
  }

  return normalized;
}

function apiUrl(serverBase: string, path: string) {
  const normalized = normalizeServerBase(serverBase);
  if (normalized && !isValidServerBase(normalized)) return path;
  return normalized ? `${normalized}${path}` : path;
}

function apiFetch(serverBase: string, path: string, init?: RequestInit) {
  return fetch(apiUrl(serverBase, path), init);
}

function HeaderNav({ currentPath }: { currentPath: string }) {
  const linkClass = (active: boolean) => `px-2 py-1 rounded text-xs transition-colors ${active ? 'bg-blue-600 text-white' : 'text-gray-400 hover:text-white hover:bg-gray-800'}`;

  return (
    <nav className="ml-auto flex items-center gap-2">
      <a href="/" className={linkClass(currentPath === '/')}>Dashboard</a>
      <a href="/health" className={linkClass(currentPath === '/health')}>Health</a>
    </nav>
  );
}

function HealthPage({ serverBase }: { serverBase: string }) {
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
    <div className="flex-1 overflow-y-auto p-4 space-y-4 bg-gray-900">
      <div className="flex items-center justify-between gap-4 bg-gray-800 border border-gray-700 rounded-lg px-4 py-3">
        <div>
          <div className="text-sm font-semibold text-white">On-demand instance health</div>
          <div className="text-xs text-gray-400">Fetches CPU, memory, swap, uptime, load average, and current server-process stats only when requested.</div>
        </div>
        <button
          onClick={loadHealth}
          disabled={loading}
          className="px-3 py-1.5 bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white text-sm rounded transition-colors"
        >
          {loading ? 'Refreshing…' : 'Fetch Health'}
        </button>
      </div>

      {!serverBase && (
        <div className="bg-amber-950/40 border border-amber-800 text-amber-300 rounded-lg px-4 py-3 text-sm">
          Set the backend server URL in the Kotak panel first if this frontend is running on a different origin than the backend.
        </div>
      )}

      {error && (
        <div className="bg-red-950/40 border border-red-800 text-red-300 rounded-lg px-4 py-3 text-sm">
          {error}
        </div>
      )}

      {!snapshot && !error && !loading && (
        <div className="bg-gray-800 border border-gray-700 rounded-lg px-4 py-6 text-sm text-gray-400">
          No snapshot loaded yet.
        </div>
      )}

      {snapshot && (
        <>
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
            <Stat icon={<Cpu size={16} className="text-blue-400" />} label="CPU Usage">
              {fmtPct(snapshot.cpu_usage_pct)}
            </Stat>
            <Stat icon={<Activity size={16} className="text-emerald-400" />} label="Load Average">
              {snapshot.load_average.one.toFixed(2)} / {snapshot.load_average.five.toFixed(2)} / {snapshot.load_average.fifteen.toFixed(2)}
            </Stat>
            <Stat icon={<HardDrive size={16} className="text-yellow-400" />} label="Memory Used">
              {snapshot.memory.used_mib} / {snapshot.memory.total_mib} MiB
            </Stat>
            <Stat icon={<HardDrive size={16} className="text-orange-400" />} label="Swap Used">
              {snapshot.swap.used_mib} / {snapshot.swap.total_mib} MiB
            </Stat>
          </div>

          <div className="grid gap-4 xl:grid-cols-[1.2fr_1fr]">
            <div className="bg-gray-800 border border-gray-700 rounded-lg overflow-hidden">
              <div className="px-4 py-2 border-b border-gray-700 text-xs text-gray-400 uppercase tracking-wide">
                Instance Overview
              </div>
              <div className="p-4 grid gap-3 sm:grid-cols-2 text-sm">
                <div><span className="text-gray-500">Generated (IST):</span> <span className="text-white">{snapshot.generated_at_ist}</span></div>
                <div><span className="text-gray-500">Hostname:</span> <span className="text-white">{snapshot.hostname ?? '—'}</span></div>
                <div><span className="text-gray-500">OS:</span> <span className="text-white">{[snapshot.os_name, snapshot.os_version].filter(Boolean).join(' ') || '—'}</span></div>
                <div><span className="text-gray-500">Kernel:</span> <span className="text-white">{snapshot.kernel_version ?? '—'}</span></div>
                <div><span className="text-gray-500">Uptime:</span> <span className="text-white">{fmtUptime(snapshot.uptime_secs)}</span></div>
                <div><span className="text-gray-500">CPU Cores:</span> <span className="text-white">{snapshot.cpu_cores}</span></div>
                <div><span className="text-gray-500">Free Memory:</span> <span className="text-white">{snapshot.memory.free_mib} MiB</span></div>
                <div><span className="text-gray-500">Free Swap:</span> <span className="text-white">{snapshot.swap.free_mib} MiB</span></div>
              </div>
            </div>

            <div className="bg-gray-800 border border-gray-700 rounded-lg overflow-hidden">
              <div className="px-4 py-2 border-b border-gray-700 text-xs text-gray-400 uppercase tracking-wide">
                Current Server Process
              </div>
              <div className="p-4 text-sm space-y-2">
                {snapshot.current_process ? (
                  <>
                    <div><span className="text-gray-500">PID:</span> <span className="text-white">{snapshot.current_process.pid}</span></div>
                    <div><span className="text-gray-500">Name:</span> <span className="text-white">{snapshot.current_process.name}</span></div>
                    <div><span className="text-gray-500">CPU:</span> <span className="text-white">{fmtPct(snapshot.current_process.cpu_usage_pct)}</span></div>
                    <div><span className="text-gray-500">Resident Memory:</span> <span className="text-white">{snapshot.current_process.memory_mib} MiB</span></div>
                    <div><span className="text-gray-500">Virtual Memory:</span> <span className="text-white">{snapshot.current_process.virtual_memory_mib} MiB</span></div>
                    <div><span className="text-gray-500">Run Time:</span> <span className="text-white">{fmtUptime(snapshot.current_process.run_time_secs)}</span></div>
                  </>
                ) : (
                  <div className="text-gray-400">Current process stats unavailable.</div>
                )}
              </div>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Top Bar — Settings
// ---------------------------------------------------------------------------

function SettingsBar({ serverBase }: { serverBase: string }) {
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
      <div className="flex items-center gap-2 text-gray-400 text-sm px-4 py-3">
        <Settings size={14} className="animate-spin" /> Loading settings…
      </div>
    );
  }

  return (
    <div className="flex flex-wrap items-end gap-4 bg-gray-900 border-b border-gray-700 px-4 py-3">
      {/* Mode toggle */}
      <div className="flex flex-col gap-1">
        <span className="text-xs text-gray-400 uppercase tracking-wide">Mode</span>
        <button
          onClick={() =>
            setCfg((c) => c && { ...c, mode: c.mode === 'PAPER' ? 'LIVE' : 'PAPER' })
          }
          className={`px-3 py-1 rounded text-xs font-bold transition-colors ${
            cfg.mode === 'LIVE'
              ? 'bg-red-600 hover:bg-red-700 text-white'
              : 'bg-emerald-700 hover:bg-emerald-600 text-white'
          }`}
        >
          {cfg.mode}
        </button>
      </div>

      {/* Numeric inputs */}
      {(
        [
          { key: 'virtual_balance', label: 'Virtual Balance (₹)' },
          { key: 'default_option_lots', label: 'Default Lots' },
          { key: 'brokerage_per_order', label: 'Brokerage (₹)' },
          { key: 'max_trade_amount_inr', label: 'Max Trade (₹)' },
          { key: 'target_1_exit_pct', label: 'Target 1 Exit %' },
          { key: 'target_2_exit_pct', label: 'Target 2 Exit %' },
        ] as { key: keyof TradingConfig | 'virtual_balance'; label: string }[]
      ).map(({ key, label }) => (
        <div key={key} className="flex flex-col gap-1">
          <label className="text-xs text-gray-400 uppercase tracking-wide">
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
            className="w-28 bg-gray-800 border border-gray-600 rounded px-2 py-1 text-sm text-white focus:outline-none focus:border-blue-500"
          />
        </div>
      ))}

      {/* Save button */}
      <button
        onClick={handleSave}
        disabled={saving}
        className="flex items-center gap-1.5 mt-5 px-3 py-1.5 bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white text-sm rounded transition-colors"
      >
        <Save size={13} />
        {saving ? 'Saving…' : saved ? '✓ Saved' : 'Save'}
      </button>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Portfolio section
// ---------------------------------------------------------------------------

function PortfolioSection({ serverBase }: { serverBase: string }) {
  const [portfolio, setPortfolio] = useState<Portfolio | null>(null);
  const [positions, setPositions] = useState<MonitoredPosition[]>([]);
  const [openTradeInfo, setOpenTradeInfo] = useState<number | null>(null);

  useEffect(() => {
    async function load() {
      try {
        const [portfolioRes, positionsRes] = await Promise.all([
          apiFetch(serverBase, '/api/portfolio'),
          apiFetch(serverBase, '/api/positions'),
        ]);
        const [portfolioJson, positionsJson] = await Promise.all([
          portfolioRes.json(),
          positionsRes.json(),
        ]);
        setPortfolio(portfolioJson);
        setPositions(Array.isArray(positionsJson) ? positionsJson : []);
      } catch (e) {
        console.error(e);
      }
    }
    load();
    const id = setInterval(load, 5_000);
    return () => clearInterval(id);
  }, [serverBase]);

  if (!portfolio) {
    return (
      <div className="flex-1 flex items-center justify-center text-gray-500 text-sm">
        Loading portfolio…
      </div>
    );
  }

  const latestPnl = portfolio.trades.reduce((acc, t) => {
    return acc + (t.action === 'BUY' ? -t.net_value : t.net_value);
  }, 0);
  const liveMtmPnl = positions
    .filter((p) => p.state === 'Active' || p.state === 'Target1Hit')
    .reduce((acc, p) => {
      if (p.signal.action !== 'BUY' || p.executed_qty <= 0 || p.ltp === undefined || p.ltp === null) {
        return acc;
      }
      return acc + (p.ltp - p.avg_buy_price) * p.executed_qty;
    }, 0);

  return (
    <div className="p-4 space-y-4">
      {/* Balance strip */}
      <div className="flex gap-4 flex-wrap">
        <Stat icon={<Wallet size={16} className="text-blue-400" />} label="Virtual Balance">
          ₹{fmt(portfolio.balance)}
        </Stat>
        <Stat
          icon={
            <TrendingUp size={16} className={latestPnl >= 0 ? 'text-emerald-400' : 'text-red-400'} />
          }
          label="Realised P&L"
        >
          <span className={latestPnl >= 0 ? 'text-emerald-400' : 'text-red-400'}>
            {latestPnl >= 0 ? '+' : ''}₹{fmt(latestPnl)}
          </span>
        </Stat>
        <Stat
          icon={
            <TrendingUp size={16} className={liveMtmPnl >= 0 ? 'text-emerald-300' : 'text-red-300'} />
          }
          label="Live MTM P&L (LTP)"
        >
          <span className={liveMtmPnl >= 0 ? 'text-emerald-300' : 'text-red-300'}>
            {liveMtmPnl >= 0 ? '+' : ''}₹{fmt(liveMtmPnl)}
          </span>
        </Stat>
        <Stat icon={<IndianRupee size={16} className="text-yellow-400" />} label="Total Trades">
          {portfolio.trades.length}
        </Stat>
      </div>

      {/* Trade history table */}
      <div className="bg-gray-800 rounded-lg overflow-hidden">
        <div className="px-4 py-2 border-b border-gray-700 text-xs text-gray-400 uppercase tracking-wide">
          Trade History
        </div>
        {portfolio.trades.length === 0 ? (
          <div className="px-4 py-6 text-center text-gray-500 text-sm">
            No trades yet — signals will appear here once the engine executes.
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-xs text-gray-400 border-b border-gray-700">
                  {['Time', 'Ticker', 'Side', 'Qty', 'Price', 'Gross', 'Charges', 'Net'].map(
                    (h) => <th key={h} className="px-3 py-2 text-left font-medium">{h}</th>,
                  )}
                </tr>
              </thead>
              <tbody>
                {portfolio.trades.map((t) => {
                  const charges = totalCharges(t);
                  const linkedPos = positions.find((p) => p.signal.instrument_name === t.ticker);
                  return (
                    <tr key={t.id} className="border-b border-gray-700/50 hover:bg-gray-700/30 transition-colors">
                      <td className="px-3 py-2 text-gray-400 text-xs whitespace-nowrap">
                        {t.timestamp.substring(0, 16).replace('T', ' ')}
                      </td>
                      <td className="px-3 py-2 font-medium text-white">
                        <div className="inline-flex items-center gap-1.5">
                          <span>{t.ticker}</span>
                          <button
                            onClick={() => setOpenTradeInfo(openTradeInfo === t.id ? null : t.id)}
                            className="text-gray-500 hover:text-blue-400"
                          >
                            <Info size={13} />
                          </button>
                        </div>
                        {openTradeInfo === t.id && (
                          <>
                            <div className="fixed inset-0 z-[190]" onClick={() => setOpenTradeInfo(null)} />
                            <div className="fixed right-6 top-24 z-[200] bg-gray-900 border border-gray-700 p-3 rounded shadow-2xl w-[min(92vw,680px)] max-h-[70vh] overflow-auto text-[10px] font-mono text-gray-300 whitespace-pre-wrap break-words">
                              {JSON.stringify({
                                trade: t,
                                signal_targets_sl: linkedPos ? linkedPos.signal : null,
                                live_position: linkedPos
                                  ? {
                                      state: linkedPos.state,
                                      current_sl: linkedPos.current_sl,
                                      ltp: linkedPos.ltp,
                                      executed_qty: linkedPos.executed_qty,
                                      avg_buy_price: linkedPos.avg_buy_price,
                                    }
                                  : null,
                              }, null, 2)}
                            </div>
                          </>
                        )}
                      </td>
                      <td className="px-3 py-2">
                        <span className={`px-1.5 py-0.5 rounded text-xs font-bold ${
                          t.action === 'BUY' ? 'bg-emerald-900 text-emerald-300' : 'bg-red-900 text-red-300'
                        }`}>{t.action}</span>
                      </td>
                      <td className="px-3 py-2 text-gray-200">{t.qty}</td>
                      <td className="px-3 py-2 text-gray-200">₹{fmt(t.executed_price)}</td>
                      <td className="px-3 py-2 text-gray-200">₹{fmt(t.gross_value)}</td>
                      <td className="px-3 py-2 text-yellow-400 text-xs">₹{fmt(charges)}</td>
                      <td className="px-3 py-2 font-medium text-white">₹{fmt(t.net_value)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}

// Small reusable stat card
function Stat({ icon, label, children }: {
  icon: React.ReactNode;
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 bg-gray-800 rounded-lg px-4 py-3">
      {icon}
      <div>
        <div className="text-xs text-gray-400">{label}</div>
        <div className="text-lg font-semibold text-white">{children}</div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Live Log Terminal
// ---------------------------------------------------------------------------

function LogTerminal({ serverBase, height = 220 }: { serverBase: string; height?: number }) {
  const [logs, setLogs] = useState<{ id: number, text: string, time: string, isError: boolean }[]>([]);
  const [filter, setFilter] = useState<'ALL' | 'ERROR'>('ALL');
  const [connected, setConnected] = useState(false);
  const [isOpen, setIsOpen] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  // Auto-scroll on new log
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [logs]);

  // SSE connection
  useEffect(() => {
    let es: EventSource | null = null;
    try {
      es = new EventSource(apiUrl(serverBase, '/api/logs/stream'));
    } catch (e) {
      console.error(e);
      setConnected(false);
      return () => {};
    }
    es.onopen = () => setConnected(true);
    es.onmessage = (e: MessageEvent<string>) => {
      const now = new Date();
      let hours = now.getHours();
      const ampm = hours >= 12 ? 'PM' : 'AM';
      hours = hours % 12;
      hours = hours ? hours : 12; // the hour '0' should be '12'
      const timeStr = `${String(hours).padStart(2, '0')}:${String(now.getMinutes()).padStart(2, '0')}:${String(now.getSeconds()).padStart(2, '0')} ${ampm}`;

      const text = e.data;
      const isError = text.includes('"level":"ERROR"') || text.includes('"event":"ERROR"');

      setLogs((prev) => [...prev.slice(-500), { id: Date.now() + Math.random(), text, time: timeStr, isError }]);
    };
    es.onerror = () => setConnected(false);
    return () => es?.close();
  }, [serverBase]);

  return (
    <div className="flex flex-col border-t border-gray-700 bg-gray-950 transition-all duration-300" style={{ height: isOpen ? height : 37 }}>
      {/* Header bar */}
      <div className="flex items-center gap-2 bg-gray-800 px-3 py-1.5 border-b border-gray-700 shrink-0">
        <button 
          onClick={() => setIsOpen(!isOpen)} 
          className="text-gray-400 hover:text-white transition-colors p-0.5 rounded hover:bg-gray-700"
        >
          {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
        </button>
        <div className={`w-2 h-2 rounded-full ${connected ? 'bg-emerald-400' : 'bg-red-500'}`} />
        {connected
          ? <Wifi size={12} className="text-emerald-400" />
          : <WifiOff size={12} className="text-red-400" />}
        <span className="text-xs text-gray-400 font-mono">
          Live Engine Log — /api/logs/stream
        </span>
        <div className="ml-4 flex gap-1 bg-gray-900 rounded p-0.5 border border-gray-700">
          <button 
            onClick={() => setFilter('ALL')} 
            className={`px-2 py-0.5 rounded text-[10px] uppercase font-bold transition-colors ${filter === 'ALL' ? 'bg-blue-600 text-white' : 'text-gray-500 hover:text-gray-300'}`}
          >All Logs</button>
          <button 
            onClick={() => setFilter('ERROR')} 
            className={`px-2 py-0.5 rounded text-[10px] uppercase font-bold transition-colors ${filter === 'ERROR' ? 'bg-red-600 text-white' : 'text-gray-500 hover:text-gray-300'}`}
          >Error Logs</button>
        </div>
        <button
          onClick={() => setLogs([])}
          className="ml-auto text-xs text-gray-500 hover:text-gray-300 transition-colors"
        >
          Clear
        </button>
      </div>

      {/* Scrollable body */}
      {isOpen && (
        <div className="flex-1 overflow-y-auto bg-gray-950 px-3 py-2 font-mono text-xs leading-5">
          {logs.length === 0 ? (
            <span className="text-gray-600">Waiting for engine events…</span>
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
                isTgMsg                         ? 'text-blue-200'
                : log.text.includes('ENTRY')          ? 'text-emerald-300'
                : log.text.includes('SL_HIT') || log.text.includes('SL_TRAILED') ? 'text-red-400'
                : log.text.includes('TGT')          ? 'text-yellow-300'
                : log.text.includes('CONFIG_UPDATED') ? 'text-blue-300'
                : log.isError                      ? 'text-red-400'
                : log.text.includes('"level":"WARN"')  ? 'text-yellow-400'
                : 'text-green-400';

              return (
                <div key={log.id} className={`${colour} flex whitespace-pre-wrap py-0.5`}>
                  <span className="text-gray-500 select-none mr-2 shrink-0">[{log.time}]</span>
                  <span className="text-gray-600 select-none mr-2 shrink-0">&gt;</span>
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

// ---------------------------------------------------------------------------
// Upcoming Trades
// ---------------------------------------------------------------------------

interface MonitoredPosition {
  id: string;
  signal: {
    instrument_name: string;
    action: string;
    entry_condition: string;
    entry_price: number;
    stop_loss: number;
    targets: number[];
  };
  state: string;
  current_sl: number;
  executed_qty: number;
  avg_buy_price: number;
  override_qty: number | null;
  resolved_order?: {
    quantity?: string;
    trading_symbol?: string;
    exchange_segment?: string;
    order_type?: string;
    product_code?: string;
    validity?: string;
    transaction_type?: string;
    trigger_price?: string;
    price?: string;
  };
  ltp?: number;
  ws_scrip_key?: string | null;
}

function QtyInput({ initialQty, id, defaultQty, onUpdate }: { initialQty: number | null, id: string, defaultQty?: string, onUpdate: (id: string, q: number | null) => void }) {
  const [val, setVal] = useState(initialQty === null ? '' : String(initialQty));
  
  useEffect(() => {
    setVal(initialQty === null ? '' : String(initialQty));
  }, [initialQty]);

  return (
    <input
      type="number"
      value={val}
      placeholder={defaultQty ? `Auto (${defaultQty})` : "Auto"}
      onChange={e => setVal(e.target.value)}
      onBlur={() => {
        const parsed = parseInt(val, 10);
        const finalVal = isNaN(parsed) ? null : parsed;
        if (finalVal !== initialQty) {
          onUpdate(id, finalVal);
        }
      }}
      className="w-20 bg-gray-900 border border-gray-600 rounded px-2 py-1 text-xs text-white placeholder-gray-600 focus:outline-none focus:border-blue-500"
    />
  );
}

function UpcomingTrades({ serverBase }: { serverBase: string }) {
  const [positions, setPositions] = useState<MonitoredPosition[]>([]);
  const [openTooltip, setOpenTooltip] = useState<string | null>(null);
  const [closingId, setClosingId] = useState<string | null>(null);

  useEffect(() => {
    function load() {
      apiFetch(serverBase, '/api/positions')
        .then(r => r.json())
        .then(setPositions)
        .catch(console.error);
    }
    load();
    const id = setInterval(load, 3000);
    return () => clearInterval(id);
  }, [serverBase]);

  async function cancelTrade(id: string) {
    try {
      await apiFetch(serverBase, `/api/positions/${id}`, { method: 'DELETE' });
      setPositions(prev => prev.filter(p => p.id !== id));
    } catch (e) {
      console.error(e);
    }
  }

  async function updateQty(id: string, qty: number | null) {
    try {
      await apiFetch(serverBase, `/api/positions/${id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ override_qty: qty }),
      });
      setPositions(prev => prev.map(p => p.id === id ? { ...p, override_qty: qty } : p));
    } catch (e) {
      console.error(e);
    }
  }

  async function closeOngoingTrade(id: string) {
    try {
      setClosingId(id);
      const res = await apiFetch(serverBase, `/api/positions/${id}/close`, { method: 'POST' });
      if (!res.ok) {
        const err = await res.json().catch(() => null);
        console.error(err?.error ?? 'Failed to close position');
        return;
      }
      setPositions((prev) => prev.filter((p) => p.id !== id));
    } catch (e) {
      console.error(e);
    } finally {
      setClosingId(null);
    }
  }

  const waiting = positions.filter((p) => p.state === 'WaitingForEntry');
  const active = positions.filter((p) => p.state === 'Active' || p.state === 'Target1Hit');

  if (waiting.length === 0 && active.length === 0) return null;

  return (
    <div className="bg-gray-800 border-b border-gray-700 shrink-0">
      {waiting.length > 0 && (
        <>
          <div className="px-4 py-2 border-b border-gray-700 text-xs text-gray-400 uppercase tracking-wide bg-gray-900 relative z-10">
            Upcoming Trades (Awaiting Entry)
          </div>
          <div>
            <table className="w-full text-sm">
              <thead>
                <tr className="text-xs text-gray-400 border-b border-gray-700">
                  <th className="px-3 py-2 text-left font-medium sticky top-0 bg-gray-800">Instrument</th>
                  <th className="px-3 py-2 text-left font-medium sticky top-0 bg-gray-800">Action</th>
                  <th className="px-3 py-2 text-left font-medium sticky top-0 bg-gray-800">Trigger</th>
                  <th className="px-3 py-2 text-left font-medium sticky top-0 bg-gray-800">SL</th>
                  <th className="px-3 py-2 text-left font-medium sticky top-0 bg-gray-800">Qty (Override)</th>
                  <th className="px-3 py-2 text-right font-medium sticky top-0 bg-gray-800">Controls</th>
                </tr>
              </thead>
              <tbody>
                {waiting.map((p) => (
                  <tr key={p.id} className="border-b border-gray-700/50 hover:bg-gray-700/30 transition-colors">
                    <td className="px-3 py-2 font-medium text-white relative">
                      <div className={`flex items-center gap-1.5 relative group/tooltip ${openTooltip === p.id ? 'z-[60]' : 'hover:z-[60]'}`}>
                        <div className="flex flex-col">
                          <span>{p.signal.instrument_name}</span>
                          {p.ltp !== undefined && p.ltp !== null && (
                            <span className="text-[10px] text-gray-400">LTP: <span className="text-blue-300">₹{fmt(p.ltp)}</span></span>
                          )}
                        </div>
                        <button
                          onClick={(e) => { e.stopPropagation(); setOpenTooltip(openTooltip === p.id ? null : p.id); }}
                          className="text-gray-500 hover:text-blue-400 focus:outline-none cursor-pointer"
                        >
                          <Info size={14} />
                        </button>

                        {openTooltip === p.id && (
                          <div
                            className="fixed inset-0 z-40 cursor-default"
                            onClick={(e) => { e.stopPropagation(); setOpenTooltip(null); }}
                          />
                        )}

                        <div className={`absolute left-full ml-2 top-1/2 -translate-y-1/2 transition-opacity bg-gray-900 border border-gray-700 p-2 rounded shadow-2xl z-[100] min-w-max text-[10px] font-mono text-gray-300 whitespace-pre ${
                          openTooltip === p.id
                            ? 'opacity-100 pointer-events-auto'
                            : 'opacity-0 group-hover/tooltip:opacity-100 pointer-events-none group-hover/tooltip:pointer-events-auto'
                        }`}>
                          <div className="text-emerald-400 font-bold mb-1 border-b border-gray-700 pb-1">Signal + Targets/SL</div>
                          {JSON.stringify(p.signal, null, 2)}

                          {p.resolved_order && (
                            <>
                              <div className="text-blue-400 font-bold mt-2 mb-1 border-b border-gray-700 pb-1">Resolved Order (Kotak API)</div>
                              {JSON.stringify(p.resolved_order, null, 2)}
                            </>
                          )}
                        </div>
                      </div>
                    </td>
                    <td className="px-3 py-2">
                      <span className={`px-1.5 py-0.5 rounded text-xs font-bold ${
                        p.signal.action === 'BUY' ? 'bg-emerald-900 text-emerald-300' : 'bg-red-900 text-red-300'
                      }`}>{p.signal.action}</span>
                    </td>
                    <td className="px-3 py-2 text-gray-200">
                      {p.signal.entry_condition} ₹{fmt(p.signal.entry_price)}
                    </td>
                    <td className="px-3 py-2 text-red-400">₹{fmt(p.signal.stop_loss)}</td>
                    <td className="px-3 py-2">
                      <QtyInput initialQty={p.override_qty} id={p.id} defaultQty={p.resolved_order?.quantity} onUpdate={updateQty} />
                    </td>
                    <td className="px-3 py-2 text-right">
                      <button onClick={() => cancelTrade(p.id)} className="px-2 py-1 bg-red-900/50 hover:bg-red-900 text-red-300 rounded text-xs transition-colors">
                        Cancel
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {active.length > 0 && (
        <>
          <div className="px-4 py-2 border-y border-gray-700 text-xs text-gray-300 uppercase tracking-wide bg-gray-900">
            Active Positions (Live LTP + MTM)
          </div>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-xs text-gray-400 border-b border-gray-700">
                  <th className="px-3 py-2 text-left font-medium">Instrument</th>
                  <th className="px-3 py-2 text-left font-medium">State</th>
                  <th className="px-3 py-2 text-left font-medium">Qty</th>
                  <th className="px-3 py-2 text-left font-medium">Entry LTP</th>
                  <th className="px-3 py-2 text-left font-medium">Current LTP</th>
                  <th className="px-3 py-2 text-left font-medium">SL</th>
                  <th className="px-3 py-2 text-left font-medium">Targets</th>
                  <th className="px-3 py-2 text-right font-medium">Unrealized P&L</th>
                  <th className="px-3 py-2 text-right font-medium">Controls</th>
                </tr>
              </thead>
              <tbody>
                {active.map((p) => {
                  const hasLtp = p.ltp !== undefined && p.ltp !== null;
                  const pnl = hasLtp ? (p.ltp! - p.avg_buy_price) * p.executed_qty : null;
                  return (
                    <tr key={p.id} className="border-b border-gray-700/50 hover:bg-gray-700/30 transition-colors">
                      <td className="px-3 py-2 font-medium text-white relative">
                        <div className={`flex items-center gap-1.5 relative ${openTooltip === p.id ? 'z-[60]' : 'hover:z-[60]'}`}>
                          <div className="flex flex-col">
                            <span>{p.signal.instrument_name}</span>
                            {p.ws_scrip_key && <span className="text-[10px] text-gray-500">{p.ws_scrip_key}</span>}
                          </div>
                          <button
                            onClick={(e) => { e.stopPropagation(); setOpenTooltip(openTooltip === p.id ? null : p.id); }}
                            className="text-gray-500 hover:text-blue-400 focus:outline-none cursor-pointer"
                          >
                            <Info size={14} />
                          </button>

                          {openTooltip === p.id && (
                            <div
                              className="fixed inset-0 z-40 cursor-default"
                              onClick={(e) => { e.stopPropagation(); setOpenTooltip(null); }}
                            />
                          )}

                          {openTooltip === p.id && (
                            <div className="fixed inset-0 z-[110] flex items-start justify-center pt-24 px-4 pointer-events-none">
                              <div className="bg-gray-900 border border-gray-700 p-3 rounded shadow-2xl text-[10px] font-mono text-gray-300 whitespace-pre max-w-[90vw] max-h-[70vh] overflow-auto pointer-events-auto">
                                <div className="text-emerald-400 font-bold mb-1 border-b border-gray-700 pb-1">Order Details + Signal</div>
                                {JSON.stringify({
                                  state: p.state,
                                  executed_qty: p.executed_qty,
                                  avg_buy_price: p.avg_buy_price,
                                  current_sl: p.current_sl,
                                  ltp: p.ltp,
                                  signal: p.signal,
                                  resolved_order: p.resolved_order,
                                }, null, 2)}
                              </div>
                            </div>
                          )}
                        </div>
                      </td>
                      <td className="px-3 py-2 text-blue-300">{p.state}</td>
                      <td className="px-3 py-2 text-gray-200">{p.executed_qty}</td>
                      <td className="px-3 py-2 text-gray-200">₹{fmt(p.avg_buy_price)}</td>
                      <td className="px-3 py-2 text-gray-100">{hasLtp ? `₹${fmt(p.ltp!)}` : '—'}</td>
                      <td className="px-3 py-2 text-red-400">₹{fmt(p.current_sl)}</td>
                      <td className="px-3 py-2 text-yellow-300">{p.signal.targets.map((t) => `₹${fmt(t)}`).join(' / ')}</td>
                      <td className="px-3 py-2 text-right">
                        {pnl === null ? (
                          <span className="text-gray-600">—</span>
                        ) : (
                          <span className={`font-semibold ${pnl >= 0 ? 'text-emerald-400' : 'text-red-400'}`}>
                            {pnl >= 0 ? '+' : ''}₹{fmt(pnl)}
                          </span>
                        )}
                      </td>
                      <td className="px-3 py-2 text-right">
                        <button
                          onClick={() => closeOngoingTrade(p.id)}
                          disabled={closingId === p.id}
                          className="px-2 py-1 bg-orange-900/60 hover:bg-orange-900 text-orange-200 rounded text-xs transition-colors disabled:opacity-50"
                        >
                          {closingId === p.id ? 'Closing…' : 'Close'}
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Connection Panel — Kotak + Telegram sign-in
// ---------------------------------------------------------------------------

// ── Kotak ────────────────────────────────────────────────────────────────── //

interface KotakForm {
  server_base: string;
  access_token: string;
  mobile_number: string;
  ucc: string;
  totp: string;
  mpin: string;
}

function KotakLoginPanel({ serverBase, onServerBaseChange }: {
  serverBase: string;
  onServerBaseChange: (value: string) => void;
}) {
  const [form, setForm] = useState<KotakForm>(() => {
    try {
      const saved = localStorage.getItem('kotak_creds');
      if (saved) {
        return {
          server_base: getStoredServerBase(),
          ...JSON.parse(saved),
          totp: '',
        } as KotakForm;
      }
    } catch {}
    return {
      server_base: getStoredServerBase(),
      access_token: '',
      mobile_number: '',
      ucc: '',
      totp: '',
      mpin: '',
    };
  });
  const [status, setStatus] = useState<'idle' | 'loading' | 'ok' | 'error'>('idle');
  const [msg, setMsg] = useState('');

  function commitServerBase(rawValue: string) {
    const normalized = normalizeServerBase(rawValue);
    if (!normalized) {
      onServerBaseChange('');
      setMsg('');
      if (status !== 'loading') setStatus('idle');
      return true;
    }

    if (!isValidServerBase(normalized)) {
      setStatus('error');
      setMsg('Enter a full http:// or https:// server URL');
      return false;
    }

    onServerBaseChange(normalized);
    if (status === 'error' && msg === 'Enter a full http:// or https:// server URL') {
      setStatus('idle');
      setMsg('');
    }
    return true;
  }

  useEffect(() => {
    setForm((current) => current.server_base === serverBase ? current : { ...current, server_base: serverBase });
  }, [serverBase]);

  useEffect(() => {
    async function checkState() {
      if (!serverBase) return;
      try {
        const res = await apiFetch(serverBase, '/api/auth/kotak');
        if (res.ok) {
          const data = await res.json();
          if (data.connected) {
            setStatus('ok');
            setMsg('Connected ✓');
          }
        }
      } catch (e) {}
    }
    checkState();
  }, [serverBase]);

  useEffect(() => {
    const { totp, server_base, ...rest } = form;
    localStorage.setItem('kotak_creds', JSON.stringify(rest));
  }, [form]);

  async function handleLogin() {
    if (!commitServerBase(form.server_base)) return;
    setStatus('loading');
    try {
      const res = await apiFetch(form.server_base, '/api/auth/kotak', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(form),
      });
      const data = await res.json();
      if (res.ok) { setStatus('ok'); setMsg('Connected ✓'); }
      else        { setStatus('error'); setMsg(data.error ?? 'Login failed'); }
    } catch (e) {
      setStatus('error');
      setMsg(String(e));
    }
  }

  const fields: { key: keyof typeof form; label: string; type?: string }[] = [
    { key: 'server_base',   label: 'Server URL or IP:PORT' },
    { key: 'access_token',  label: 'API Access Token' },
    { key: 'mobile_number', label: 'Mobile (+91…)' },
    { key: 'ucc',           label: 'UCC (Client Code)' },
    { key: 'totp',          label: 'TOTP (6 digits)', type: 'text' },
    { key: 'mpin',          label: 'MPIN (6 digits)', type: 'password' },
  ];

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 text-xs font-semibold text-gray-300 uppercase tracking-wide">
        <KeyRound size={12} className="text-orange-400" /> Kotak Neo Login
      </div>
      <div className="flex flex-wrap gap-2">
        {fields.map(({ key, label, type }) => (
          <input
            key={key}
            type={type ?? 'text'}
            placeholder={label}
            value={form[key]}
            onChange={e => setForm(f => ({ ...f, [key]: e.target.value }))}
            onBlur={key === 'server_base' ? (e) => { void commitServerBase(e.target.value); } : undefined}
            className="w-36 bg-gray-800 border border-gray-600 rounded px-2 py-1 text-xs text-white placeholder-gray-500 focus:outline-none focus:border-orange-500"
          />
        ))}
        <button
          onClick={handleLogin}
          disabled={status === 'loading' || status === 'ok'}
          className="flex items-center gap-1 px-3 py-1 bg-orange-600 hover:bg-orange-500 disabled:opacity-50 text-white text-xs rounded transition-colors"
        >
          <Plug size={11} />
          {status === 'loading' ? 'Connecting…' : status === 'ok' ? 'Connected' : 'Connect'}
        </button>
        {msg && (
          <span className={`text-xs self-center ${status === 'ok' ? 'text-emerald-400' : 'text-red-400'}`}>
            {msg}
          </span>
        )}
        {status === 'ok' && (
          <div className="flex gap-2 ml-auto">
            <a href={apiUrl(form.server_base, '/api/auth/kotak/scrip-master/raw')} download="scrip_master.csv" className="text-[10px] text-blue-400 hover:text-blue-300 underline self-center">Download CSV</a>
            <a href={apiUrl(form.server_base, '/api/auth/kotak/scrip-master/json')} target="_blank" rel="noreferrer" className="text-[10px] text-blue-400 hover:text-blue-300 underline self-center">View JSON</a>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Telegram ─────────────────────────────────────────────────────────────── //

type TgStep = 'idle' | 'code' | 'twofa' | 'chats' | 'running';

interface TelegramChat { id: number; name: string; kind: string }

function TelegramLoginPanel({ serverBase }: { serverBase: string }) {
  const [step, setStep] = useState<TgStep>('idle');
  const [apiId, setApiId]     = useState(() => localStorage.getItem('tg_api_id') || '');
  const [apiHash, setApiHash] = useState(() => localStorage.getItem('tg_api_hash') || '');
  const [phone, setPhone]     = useState(() => localStorage.getItem('tg_phone') || '');
  const [code, setCode]       = useState('');
  const [twofa, setTwofa]     = useState(() => localStorage.getItem('tg_twofa') || '');
  const [chats, setChats]     = useState<TelegramChat[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [err, setErr]         = useState('');

  useEffect(() => {
    async function checkState() {
      try {
        const res = await apiFetch(serverBase, '/api/auth/telegram/status');
        if (res.ok) {
          const data = await res.json();
          if (data.state === 'running') {
            setStep('running');
            if (Array.isArray(data.chat_ids)) {
              setSelected(new Set(data.chat_ids));
            }
          }
          else if (data.state === 'authenticated') loadChats();
        }
      } catch (e) {}
    }
    checkState();
  }, [serverBase]);

  useEffect(() => { localStorage.setItem('tg_api_id', apiId); }, [apiId]);
  useEffect(() => { localStorage.setItem('tg_api_hash', apiHash); }, [apiHash]);
  useEffect(() => { localStorage.setItem('tg_phone', phone); }, [phone]);
  useEffect(() => { localStorage.setItem('tg_twofa', twofa); }, [twofa]);

  async function post(url: string, body: object) {
    const res = await apiFetch(serverBase, url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    return res.json();
  }

  async function requestCode() {
    setErr('');
    const data = await post('/api/auth/telegram/request-code', {
      api_id: parseInt(apiId, 10), api_hash: apiHash, phone,
    });
    if (data.error) { setErr(data.error); return; }
    if (data.status === 'authenticated') {
      await loadChats();
    } else {
      setStep('code');
    }
  }

  async function submitCode() {
    setErr('');
    const data = await post('/api/auth/telegram/submit-code', { code });
    if (data.error) { setErr(data.error); return; }
    if (data.twofa_required) { setStep('twofa'); }
    else { await loadChats(); }
  }

  async function submit2fa() {
    setErr('');
    const data = await post('/api/auth/telegram/submit-2fa', { password: twofa });
    if (data.error) { setErr(data.error); return; }
    await loadChats();
  }

  async function loadChats() {
    const data: TelegramChat[] | { error: string } = await apiFetch(serverBase, '/api/auth/telegram/chats').then(r => r.json());
    if ('error' in data) { setErr(data.error); return; }
    setChats(data);
    setStep('chats');
  }

  async function startMonitoring() {
    setErr('');
    const data = await post('/api/auth/telegram/start', { chat_ids: [...selected] });
    if (data.error) { setErr(data.error); return; }
    setStep('running');
  }

  const kindIcon = (k: string) =>
    k === 'user' ? '👤' : k === 'channel' ? '📢' : k === 'community' ? '🏛' : '👥';

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 text-xs font-semibold text-gray-300 uppercase tracking-wide">
        <MessageCircle size={12} className="text-blue-400" /> Telegram Userbot
        {step === 'running' && <span className="text-emerald-400 normal-case font-normal">(monitoring)</span>}
      </div>

      {/* Step: idle — enter credentials */}
      {step === 'idle' && (
        <div className="flex flex-wrap gap-2">
          <input value={apiId}   onChange={e => setApiId(e.target.value)}
            placeholder="API ID" className="w-24 input-sm" />
          <input value={apiHash} onChange={e => setApiHash(e.target.value)}
            placeholder="API Hash" className="w-52 input-sm" />
          <input value={phone}   onChange={e => setPhone(e.target.value)}
            placeholder="+91XXXXXXXXXX" className="w-36 input-sm" />
          <button onClick={requestCode}
            className="btn-sm bg-blue-600 hover:bg-blue-500">
            Send Code
          </button>
        </div>
      )}

      {/* Step: code — enter the 5-digit code */}
      {step === 'code' && (
        <div className="flex flex-wrap gap-2 items-center">
          <span className="text-xs text-gray-400">Code sent to {phone}:</span>
          <input value={code} onChange={e => setCode(e.target.value)}
            placeholder="12345" maxLength={10} className="w-24 input-sm" />
          <button onClick={submitCode} className="btn-sm bg-blue-600 hover:bg-blue-500">
            Confirm
          </button>
          <button onClick={() => setStep('idle')} className="btn-sm bg-gray-700 hover:bg-gray-600">
            Back
          </button>
        </div>
      )}

      {/* Step: 2FA */}
      {step === 'twofa' && (
        <div className="flex flex-wrap gap-2 items-center">
          <span className="text-xs text-gray-400">2FA password:</span>
          <input value={twofa} type="password" onChange={e => setTwofa(e.target.value)}
            placeholder="password" className="w-36 input-sm" />
          <button onClick={submit2fa} className="btn-sm bg-blue-600 hover:bg-blue-500">
            Confirm
          </button>
        </div>
      )}

      {/* Step: chats — multi-select which groups to monitor */}
      {step === 'chats' && (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-gray-400">
            Select groups/channels to monitor ({selected.size} selected):
          </p>
          <div className="flex flex-wrap gap-1 max-h-28 overflow-y-auto pr-1">
            {chats.map(c => {
              const on = selected.has(c.id);
              return (
                <button
                  key={c.id}
                  onClick={() => {
                    setSelected(prev => {
                      const s = new Set(prev);
                      on ? s.delete(c.id) : s.add(c.id);
                      return s;
                    });
                  }}
                  className={`flex items-center gap-1 px-2 py-0.5 rounded text-xs border transition-colors ${
                    on
                      ? 'bg-blue-700 border-blue-500 text-white'
                      : 'bg-gray-800 border-gray-600 text-gray-300 hover:border-blue-500'
                  }`}
                >
                  {kindIcon(c.kind)} {c.name}
                  <span className="text-gray-500 text-[10px]">({c.id})</span>
                </button>
              );
            })}
          </div>
          <div className="flex gap-2">
            <button
              onClick={startMonitoring}
              disabled={selected.size === 0}
              className="btn-sm bg-emerald-700 hover:bg-emerald-600 disabled:opacity-40"
            >
              Start Monitoring ({selected.size})
            </button>
            <button onClick={() => { setStep('idle'); setChats([]); setSelected(new Set()); }}
              className="btn-sm bg-gray-700 hover:bg-gray-600">
              Reset
            </button>
          </div>
        </div>
      )}

      {/* Running state */}
      {step === 'running' && (
        <div className="flex items-center gap-3 text-xs">
          <span className="text-emerald-400">● Monitoring {selected.size} chat(s)</span>
          <button onClick={() => { setStep('idle'); setSelected(new Set()); setChats([]); }}
            className="btn-sm bg-gray-700 hover:bg-gray-600">
            Disconnect
          </button>
        </div>
      )}

      {err && <p className="text-xs text-red-400">{err}</p>}
    </div>
  );
}

// ── Combined panel ────────────────────────────────────────────────────────── //

function ConnectionPanel({ serverBase, onServerBaseChange }: {
  serverBase: string;
  onServerBaseChange: (value: string) => void;
}) {
  const [sysStatus, setSysStatus] = useState({ telegram_connected: false, kotak_connected: false });

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

  return (
    <div className="flex flex-col gap-4 bg-gray-900 border-b border-gray-700 px-4 py-3">
      <div className="flex items-center justify-between">
        <div className="flex gap-4 text-xs font-semibold">
          <div className="flex items-center gap-1">
            <div className={`w-2 h-2 rounded-full ${sysStatus.kotak_connected ? 'bg-emerald-500 shadow-[0_0_5px_rgba(16,185,129,0.5)]' : 'bg-red-500'}`} />
            <span className={sysStatus.kotak_connected ? 'text-gray-200' : 'text-gray-500'}>Kotak</span>
          </div>
          <div className="flex items-center gap-1">
            <div className={`w-2 h-2 rounded-full ${sysStatus.telegram_connected ? 'bg-emerald-500 shadow-[0_0_5px_rgba(16,185,129,0.5)]' : 'bg-red-500'}`} />
            <span className={sysStatus.telegram_connected ? 'text-gray-200' : 'text-gray-500'}>Telegram</span>
          </div>
        </div>
        <button
          onClick={handleReset}
          className="btn-sm bg-red-900/40 hover:bg-red-800/60 text-red-400 border border-red-900/50 hover:text-white transition-colors"
        >
          Reset Connections
        </button>
      </div>
      <KotakLoginPanel serverBase={serverBase} onServerBaseChange={onServerBaseChange} />
      <TelegramLoginPanel serverBase={serverBase} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

export default function App() {
  const [logHeight, setLogHeight] = useState(220);
  const [serverBase, setServerBase] = useState(() => getStoredServerBase());
  const currentPath = typeof window === 'undefined' ? '/' : window.location.pathname;
  const isHealthPage = currentPath === '/health';

  function handleServerBaseChange(value: string) {
    setServerBase(persistServerBase(value));
  }

  return (
    <div className="flex flex-col h-screen bg-gray-900 text-white overflow-hidden">
      <div className="flex-1 overflow-y-auto flex flex-col">
        <header className="flex items-center gap-3 px-4 py-2 bg-gray-950 border-b border-gray-800 shrink-0">
          <Activity size={18} className="text-blue-400" />
          <h1 className="text-sm font-semibold tracking-wide">
            Auto Trader <span className="text-gray-500 font-normal">— Options OMS</span>
          </h1>
          <HeaderNav currentPath={currentPath} />
        </header>
        {isHealthPage ? (
          <HealthPage serverBase={serverBase} />
        ) : (
          <>
            <div className="shrink-0"><SettingsBar serverBase={serverBase} /></div>
            <div className="shrink-0"><ConnectionPanel serverBase={serverBase} onServerBaseChange={handleServerBaseChange} /></div>
            <UpcomingTrades serverBase={serverBase} />
            <PortfolioSection serverBase={serverBase} />
          </>
        )}
      </div>
      {!isHealthPage && (
        <div className="shrink-0 relative group">
          <div 
            className="absolute top-0 left-0 right-0 h-1 cursor-ns-resize hover:bg-blue-500/50 z-10 transition-colors"
            onMouseDown={(e) => {
              e.preventDefault();
              const startY = e.clientY;
              const startH = logHeight;
              const onMove = (ev: MouseEvent) => {
                const diff = startY - ev.clientY;
                setLogHeight(Math.max(100, Math.min(window.innerHeight - 100, startH + diff)));
              };
              const onUp = () => {
                window.removeEventListener('mousemove', onMove);
                window.removeEventListener('mouseup', onUp);
              };
              window.addEventListener('mousemove', onMove);
              window.addEventListener('mouseup', onUp);
            }}
          />
          <LogTerminal serverBase={serverBase} height={logHeight} />
        </div>
      )}
    </div>
  );
}
