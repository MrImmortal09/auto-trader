import { useState, useEffect } from 'react';
import {
  BarChart2,
  LayoutDashboard,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Settings,
  TrendingUp,
  Wallet,
} from 'lucide-react';
import type { ScreenId } from '../../types';

export function SideNavBar({
  activeScreen,
  onSelectScreen,
  mode
}: {
  activeScreen: ScreenId;
  onSelectScreen: (screen: ScreenId) => void;
  mode: string;
}) {
  const [isCollapsed, setIsCollapsed] = useState(false);

  useEffect(() => {
    try {
      const saved = localStorage.getItem('auto_trader_sidebar_collapsed');
      if (saved === 'true') setIsCollapsed(true);
    } catch {
      // ignore localStorage error
    }
  }, []);

  const toggleCollapse = () => {
    setIsCollapsed((prev) => {
      const next = !prev;
      try {
        localStorage.setItem('auto_trader_sidebar_collapsed', String(next));
      } catch {
        // ignore
      }
      return next;
    });
  };

  const itemClass = (active: boolean) =>
    `flex items-center ${
      isCollapsed ? 'justify-center px-0 py-3' : 'gap-3 px-3.5 py-2.5'
    } rounded-lg text-sm font-medium transition-all cursor-pointer select-none ${
      active
        ? 'bg-surface-container-high text-primary font-bold shadow-sm'
        : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
    }`;

  return (
    <aside
      className={`hidden md:flex ${
        isCollapsed ? 'w-20' : 'w-60'
      } bg-surface border-r border-outline-variant flex-col h-screen py-6 shrink-0 z-30 select-none transition-all duration-300 ease-in-out`}
    >
      <div className={`${isCollapsed ? 'px-3' : 'px-5'} mb-6 flex items-center justify-between`}>
        {!isCollapsed ? (
          <>
            <div className="flex items-center gap-3">
              <div
                onClick={toggleCollapse}
                title="Click to collapse sidebar"
                className="w-10 h-10 rounded-lg bg-primary-container flex items-center justify-center text-on-primary font-bold text-lg shadow-sm cursor-pointer hover:opacity-90 transition-opacity"
              >
                AT
              </div>
              <div>
                <h1 className="text-base font-bold text-on-surface leading-tight">Auto Trader</h1>
                <p className="text-[11px] font-semibold text-on-surface-variant uppercase tracking-wider">Options OMS</p>
              </div>
            </div>
            <button
              onClick={toggleCollapse}
              title="Collapse Sidebar"
              className="p-1.5 rounded-lg text-on-surface-variant hover:text-on-surface hover:bg-surface-container transition-colors"
            >
              <PanelLeftClose size={18} />
            </button>
          </>
        ) : (
          <div className="w-full flex justify-center">
            <button
              onClick={toggleCollapse}
              title="Expand Sidebar"
              className="w-10 h-10 rounded-lg bg-primary-container hover:bg-primary flex items-center justify-center text-on-primary font-bold text-lg shadow-sm transition-colors relative group"
            >
              AT
              <span className="absolute -bottom-1 -right-1 bg-surface-container-high border border-outline-variant rounded-full p-0.5 text-on-surface shadow-sm">
                <PanelLeftOpen size={10} />
              </span>
            </button>
          </div>
        )}
      </div>

      <nav className="flex-1 px-3 space-y-1">
        <div
          onClick={() => onSelectScreen('dashboard')}
          title="Dashboard"
          className={itemClass(activeScreen === 'dashboard')}
        >
          <LayoutDashboard size={isCollapsed ? 20 : 18} className="shrink-0" />
          {!isCollapsed && <span>Dashboard</span>}
        </div>
        <div
          onClick={() => onSelectScreen('positions')}
          title="Positions"
          className={itemClass(activeScreen === 'positions')}
        >
          <Wallet size={isCollapsed ? 20 : 18} className="shrink-0" />
          {!isCollapsed && <span>Positions</span>}
        </div>
        <div
          onClick={() => onSelectScreen('analytics')}
          title="Analytics"
          className={itemClass(activeScreen === 'analytics')}
        >
          <BarChart2 size={isCollapsed ? 20 : 18} className="shrink-0" />
          {!isCollapsed && <span>Analytics</span>}
        </div>
        <div
          onClick={() => onSelectScreen('portfolio')}
          title="Portfolio"
          className={itemClass(activeScreen === 'portfolio')}
        >
          <TrendingUp size={isCollapsed ? 20 : 18} className="shrink-0" />
          {!isCollapsed && <span>Portfolio</span>}
        </div>
        <div
          onClick={() => onSelectScreen('settings')}
          title="Settings"
          className={itemClass(activeScreen === 'settings')}
        >
          <Settings size={isCollapsed ? 20 : 18} className="shrink-0" />
          {!isCollapsed && <span>Settings</span>}
        </div>
      </nav>

      <div className={`mt-auto pt-6 border-t border-outline-variant space-y-3 ${isCollapsed ? 'px-3' : 'px-5'}`}>
        {!isCollapsed ? (
          <div className="flex items-center justify-between text-xs font-semibold px-1">
            <span className="text-on-surface-variant uppercase tracking-wider">Trading Mode</span>
            <span
              className={`px-2 py-0.5 rounded uppercase font-bold text-[11px] ${
                mode === 'LIVE'
                  ? 'bg-error text-on-error'
                  : 'bg-secondary text-on-secondary'
              }`}
            >
              {mode}
            </span>
          </div>
        ) : (
          <div className="flex justify-center" title={`Trading Mode: ${mode}`}>
            <span
              className={`px-1.5 py-0.5 rounded uppercase font-bold text-[10px] ${
                mode === 'LIVE'
                  ? 'bg-error text-on-error'
                  : 'bg-secondary text-on-secondary'
              }`}
            >
              {mode === 'LIVE' ? 'LIVE' : 'PPR'}
            </span>
          </div>
        )}

        <button
          onClick={() => onSelectScreen('positions')}
          title="New Trade"
          className={`w-full bg-primary-container hover:bg-primary text-on-primary font-bold py-2 rounded-lg transition-colors text-sm shadow-sm flex items-center justify-center gap-2 ${
            isCollapsed ? 'px-0 py-2.5' : 'px-4'
          }`}
        >
          {isCollapsed ? <Plus size={20} /> : <span>New Trade</span>}
        </button>
      </div>
    </aside>
  );
}
