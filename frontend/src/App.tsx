import { useEffect, useState } from 'react';
import type { ScreenId } from './types';
import { apiFetch, getStoredServerBase, persistServerBase } from './lib/api';
import { fmt, todayIsoIST } from './lib/format';
import { usePortfolioSnapshot } from './hooks/usePortfolioSnapshot';

// Layout
import { SideNavBar } from './components/layout/SideNavBar';
import { TopNavBar } from './components/layout/TopNavBar';
import { BottomNavBar } from './components/layout/BottomNavBar';

// Components
import { Info } from 'lucide-react';
import { LogTerminal } from './components/LogTerminal';
import { UpcomingTrades } from './components/UpcomingTrades';
import { TelegramFeed } from './components/TelegramFeed';
import { PortfolioSection } from './components/PortfolioSection';
import { KotakLoginPanel } from './components/KotakLoginPanel';
import { TelegramLoginPanel } from './components/TelegramLoginPanel';
import { ConnectionPanel } from './components/ConnectionPanel';
import { SettingsBar } from './components/SettingsBar';
import { HealthPage } from './components/HealthPage';

// Screens
import { TradeAnalyticsScreen } from './screens/TradeAnalyticsScreen';
import { PortfolioPerformanceScreen } from './screens/PortfolioPerformanceScreen';

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
      if (p.startsWith('/positions')) return 'positions';
    }
    return 'dashboard';
  });

  useEffect(() => {
    apiFetch(serverBase, '/api/settings')
      .then((r) => r.json())
      .then((data) => {
        if (data?.mode) setMode(data.mode);
      })
      .catch(() => {});
  }, [serverBase]);

  function handleSelectScreen(screen: ScreenId) {
    setActiveScreen(screen);
    if (typeof window !== 'undefined') {
      const paths: Record<ScreenId, string> = {
        dashboard: '/',
        positions: '/positions',
        analytics: '/analytics',
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
        />

        {/* Dynamic Screen View */}
        <main className="flex-1 overflow-y-auto p-4 sm:p-6 md:p-8 pb-24 md:pb-8 space-y-6 bg-surface">
          {activeScreen === 'dashboard' && (
            <>
              {/* Key Metrics Header */}
              <div className="grid grid-cols-2 lg:grid-cols-4 gap-3 sm:gap-4">
                <div className="bg-surface-container-lowest rounded-xl border border-outline-variant p-3 sm:p-4 shadow-sm flex flex-col justify-between">
                  <div className="flex items-center justify-between text-[11px] sm:text-xs text-on-surface-variant font-semibold uppercase tracking-wider">
                    <span>Virtual Balance</span>
                    <Info size={14} className="text-outline cursor-help" />
                  </div>
                  <span className="text-lg sm:text-xl font-bold text-on-surface tabular-nums mt-1">₹{fmt(portfolio?.balance ?? 0)}</span>
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

          {activeScreen === 'portfolio' && (
            <PortfolioPerformanceScreen serverBase={serverBase} />
          )}

          {activeScreen === 'settings' && (
            <div className="grid grid-cols-1 xl:grid-cols-2 gap-6 items-start">
              <div className="space-y-6">
                <KotakLoginPanel serverBase={serverBase} onServerBaseChange={handleServerBaseChange} />
                <TelegramLoginPanel serverBase={serverBase} />
                <ConnectionPanel serverBase={serverBase} onServerBaseChange={handleServerBaseChange} />
              </div>
              <div className="space-y-6">
                <SettingsBar serverBase={serverBase} />
                <div className="bg-surface-container-lowest border border-outline-variant rounded-xl p-4 sm:p-6 shadow-sm">
                  <h3 className="text-base font-bold text-on-surface mb-4">System Uptime &amp; Health Snapshot</h3>
                  <HealthPage serverBase={serverBase} />
                </div>
              </div>
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
    </div>
  );
}
