import { useEffect, useState } from 'react';
import type { ScreenId } from './types';
import { apiFetch, getStoredServerBase, persistServerBase } from './lib/api';
import { balanceLabel, fmt, todayIsoIST } from './lib/format';
import { usePortfolioSnapshot } from './hooks/usePortfolioSnapshot';

// Layout
import { SideNavBar } from './components/layout/SideNavBar';
import { TopNavBar } from './components/layout/TopNavBar';
import { BottomNavBar } from './components/layout/BottomNavBar';

// Components
import { Info, Plug, SlidersHorizontal, Activity, AlertOctagon } from 'lucide-react';
import { LogTerminal } from './components/LogTerminal';
import { UpcomingTrades } from './components/UpcomingTrades';
import { TelegramFeed } from './components/TelegramFeed';
import { PortfolioSection } from './components/PortfolioSection';
import { ConnectionPanel } from './components/ConnectionPanel';
import { SettingsBar } from './components/SettingsBar';
import { HealthPage } from './components/HealthPage';
import { KillSwitchModal } from './components/KillSwitchModal';

// Screens
import { TradeAnalyticsScreen } from './screens/TradeAnalyticsScreen';
import { PortfolioPerformanceScreen } from './screens/PortfolioPerformanceScreen';
import { MarketDataScreen } from './screens/MarketDataScreen';

export default function App() {
  const [logHeight, setLogHeight] = useState(220);
  const [serverBase, setServerBase] = useState(() => getStoredServerBase());
  const [mode, setMode] = useState<string>('PAPER');
  const { portfolio, realizedPnl, liveMtmPnl } = usePortfolioSnapshot(serverBase);
  const totalTradesToday = portfolio?.trades.filter((t) => t.timestamp.startsWith(todayIsoIST())).length ?? 0;

  const [activeScreen, setActiveScreen] = useState<ScreenId>(() => {
    if (typeof window !== 'undefined') {
      const p = window.location.pathname;
      if (p.startsWith('/reports') || p.startsWith('/portfolio')) return 'portfolio';
      if (p.startsWith('/health') || p.startsWith('/settings')) return 'settings';
      if (p.startsWith('/analytics')) return 'analytics';
      if (p.startsWith('/market')) return 'market';
      if (p.startsWith('/positions')) return 'positions';
    }
    return 'dashboard';
  });

  const [killSwitchOpen, setKillSwitchOpen] = useState(false);
  const [killSwitchActive, setKillSwitchActive] = useState(false);

  useEffect(() => {
    const handleServerBaseChange = (e: Event) => {
      const customEvent = e as CustomEvent<{ serverBase: string }>;
      if (typeof customEvent.detail?.serverBase === 'string') {
        setServerBase(customEvent.detail.serverBase);
      }
    };
    if (typeof window !== 'undefined') {
      window.addEventListener('at_server_base_change', handleServerBaseChange);
    }
    return () => {
      if (typeof window !== 'undefined') {
        window.removeEventListener('at_server_base_change', handleServerBaseChange);
      }
    };
  }, []);

  useEffect(() => {
    function checkStatus() {
      apiFetch(serverBase, '/api/settings')
        .then((r) => r.json())
        .then((data) => {
          if (data?.mode) setMode(data.mode);
          if (data?.kill_switch_active !== undefined) {
            setKillSwitchActive(Boolean(data.kill_switch_active));
          }
        })
        .catch(() => {});
    }

    checkStatus();
    const timer = setInterval(checkStatus, 4000);
    return () => clearInterval(timer);
  }, [serverBase]);

  function handleSelectScreen(screen: ScreenId) {
    setActiveScreen(screen);
    if (typeof window !== 'undefined') {
      const paths: Record<ScreenId, string> = {
        dashboard: '/',
        positions: '/positions',
        analytics: '/analytics',
        market: '/market',
        portfolio: '/portfolio',
        settings: '/settings',
      };
      window.history.pushState(null, '', paths[screen]);
    }
  }

  function handleServerBaseChange(value: string) {
    setServerBase(persistServerBase(value));
  }

  return (
    <div className="flex h-screen bg-surface text-on-surface overflow-hidden">
      {/* Institutional Left Sidebar */}
      <SideNavBar
        activeScreen={activeScreen}
        onSelectScreen={handleSelectScreen}
        mode={mode}
      />

      {/* Main Content Area */}
      <div className="flex-1 flex flex-col overflow-hidden">
        {/* Top Navbar */}
        <TopNavBar
          activeScreen={activeScreen}
          serverBase={serverBase}
          onNewTrade={() => handleSelectScreen('positions')}
          onOpenKillSwitch={() => setKillSwitchOpen(true)}
          killSwitchActive={killSwitchActive}
        />

        {/* Persistent Emergency Kill Switch Banner */}
        {killSwitchActive && (
          <div className="bg-red-500/15 border-b border-red-500/30 px-4 py-2.5 flex items-center justify-between gap-3 text-red-400 text-xs font-semibold shrink-0">
            <div className="flex items-center gap-2 min-w-0">
              <AlertOctagon size={16} className="text-red-400 shrink-0 animate-pulse" />
              <span className="truncate sm:whitespace-normal">
                EMERGENCY KILL SWITCH ENGAGED: Automated entries are halted &amp; all holdings liquidated.
              </span>
            </div>
            <button
              onClick={() => setKillSwitchOpen(true)}
              className="px-2.5 py-1 rounded-md bg-red-500/20 hover:bg-red-500/30 border border-red-500/40 text-red-300 font-bold text-[11px] transition-colors shrink-0"
            >
              Manage / Resume
            </button>
          </div>
        )}

        {/* Dynamic Screen View */}
        <main className="flex-1 overflow-y-auto p-4 sm:p-6 md:p-8 pb-24 md:pb-8 space-y-6 bg-surface">
          {activeScreen === 'dashboard' && (
            <>
              {/* Key Metrics Header */}
              <div className="grid grid-cols-2 lg:grid-cols-4 gap-3 sm:gap-4">
                <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-3 sm:p-4 shadow-sm flex flex-col justify-between">
                  <div className="flex items-center justify-between text-[11px] sm:text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
                    <span>{balanceLabel(portfolio?.balance_source)}</span>
                    <Info size={14} className="text-outline cursor-help" />
                  </div>
                  <span className="text-lg sm:text-xl font-bold text-on-surface tabular-nums mt-1">
                    {portfolio?.balance_source === 'LIVE_UNAVAILABLE' ? '—' : `₹${fmt(portfolio?.balance ?? 0)}`}
                  </span>
                </div>
                <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-3 sm:p-4 shadow-sm flex flex-col justify-between">
                  <div className="flex items-center justify-between text-[11px] sm:text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
                    <span>Realized P&amp;L</span>
                    <Info size={14} className="text-outline cursor-help" />
                  </div>
                  <span className={`text-lg sm:text-xl font-bold tabular-nums mt-1 ${realizedPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                    {realizedPnl >= 0 ? '+' : ''}₹{fmt(realizedPnl)}
                  </span>
                </div>
                <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-3 sm:p-4 shadow-sm flex flex-col justify-between">
                  <div className="flex items-center justify-between text-[11px] sm:text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
                    <span>Live MTM (LTP)</span>
                    <Info size={14} className="text-outline cursor-help" />
                  </div>
                  <span className={`text-lg sm:text-xl font-bold tabular-nums mt-1 ${liveMtmPnl >= 0 ? 'text-secondary' : 'text-error'}`}>
                    {liveMtmPnl >= 0 ? '+' : ''}₹{fmt(liveMtmPnl)}
                  </span>
                </div>
                <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-3 sm:p-4 shadow-sm flex flex-col justify-between">
                  <div className="flex items-center justify-between text-[11px] sm:text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
                    <span>Trades Today</span>
                    <Info size={14} className="text-outline cursor-help" />
                  </div>
                  <span className="text-lg sm:text-xl font-bold text-on-surface tabular-nums mt-1">{totalTradesToday}</span>
                </div>
              </div>

              {/* Active Positions Table */}
              <UpcomingTrades serverBase={serverBase} />

              {/* Trade History Table */}
              <PortfolioSection serverBase={serverBase} />
            </>
          )}

          {activeScreen === 'positions' && (
            <>
              <UpcomingTrades serverBase={serverBase} />
              <TelegramFeed serverBase={serverBase} />
            </>
          )}

          {activeScreen === 'analytics' && (
            <TradeAnalyticsScreen serverBase={serverBase} />
          )}

          {activeScreen === 'market' && (
            <MarketDataScreen serverBase={serverBase} />
          )}

          {activeScreen === 'portfolio' && (
            <PortfolioPerformanceScreen serverBase={serverBase} />
          )}

          {activeScreen === 'settings' && (
            <div className="space-y-10">
              <section className="space-y-4">
                <h2 className="flex items-center gap-2 text-lg font-bold text-on-surface">
                  <Plug size={18} className="text-primary" /> Connections
                </h2>
                <ConnectionPanel serverBase={serverBase} onServerBaseChange={handleServerBaseChange} />
              </section>

              <section className="space-y-4">
                <h2 className="flex items-center gap-2 text-lg font-bold text-on-surface">
                  <SlidersHorizontal size={18} className="text-primary" /> Trading Configuration
                </h2>
                <SettingsBar serverBase={serverBase} />
              </section>

              <section className="space-y-4">
                <h2 className="flex items-center gap-2 text-lg font-bold text-on-surface">
                  <Activity size={18} className="text-primary" /> System Health
                </h2>
                <HealthPage serverBase={serverBase} />
              </section>
            </div>
          )}
        </main>

        {/* Collapsible Live Engine Log Terminal */}
        <div className="shrink-0 relative group border-t border-outline-variant mb-14 md:mb-0">
          <div
            className="absolute -top-1 left-0 right-0 h-2 cursor-ns-resize hover:bg-primary/50 z-10 transition-colors"
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
      </div>

      {/* iPhone 17 Bottom Navigation Bar */}
      <BottomNavBar
        activeScreen={activeScreen}
        onSelectScreen={handleSelectScreen}
      />

      {/* Emergency Kill Switch Modal */}
      <KillSwitchModal
        serverBase={serverBase}
        isOpen={killSwitchOpen}
        onClose={() => setKillSwitchOpen(false)}
        onSuccess={(active) => setKillSwitchActive(active)}
        isAlreadyActive={killSwitchActive}
      />
    </div>
  );
}
