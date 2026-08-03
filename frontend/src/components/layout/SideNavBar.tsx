import {
  BarChart2,
  LayoutDashboard,
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
  const itemClass = (active: boolean) =>
    `flex items-center gap-3 px-3.5 py-2.5 rounded-lg text-sm font-medium transition-all cursor-pointer select-none ${
      active
        ? 'bg-surface-container-high text-primary font-bold shadow-sm'
        : 'text-on-surface-variant hover:bg-surface-container hover:text-on-surface'
    }`;

  return (
    <aside className="hidden md:flex w-60 bg-surface border-r border-outline-variant flex-col h-screen py-6 shrink-0 z-30 select-none">
      <div className="px-6 mb-8 flex items-center gap-3">
        <div className="w-10 h-10 rounded-lg bg-primary-container flex items-center justify-center text-on-primary font-bold text-lg shadow-sm">
          AT
        </div>
        <div>
          <h1 className="text-base font-bold text-on-surface leading-tight">Auto Trader</h1>
          <p className="text-[11px] font-semibold text-on-surface-variant uppercase tracking-wider">Options OMS</p>
        </div>
      </div>

      <nav className="flex-1 px-3 space-y-1">
        <div onClick={() => onSelectScreen('dashboard')} className={itemClass(activeScreen === 'dashboard')}>
          <LayoutDashboard size={18} />
          <span>Dashboard</span>
        </div>
        <div onClick={() => onSelectScreen('positions')} className={itemClass(activeScreen === 'positions')}>
          <Wallet size={18} />
          <span>Positions</span>
        </div>
        <div onClick={() => onSelectScreen('analytics')} className={itemClass(activeScreen === 'analytics')}>
          <BarChart2 size={18} />
          <span>Analytics</span>
        </div>
        <div onClick={() => onSelectScreen('portfolio')} className={itemClass(activeScreen === 'portfolio')}>
          <TrendingUp size={18} />
          <span>Portfolio</span>
        </div>
        <div onClick={() => onSelectScreen('settings')} className={itemClass(activeScreen === 'settings')}>
          <Settings size={18} />
          <span>Settings</span>
        </div>
      </nav>

      <div className="px-6 mt-auto pt-6 border-t border-outline-variant space-y-3">
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
        <button
          onClick={() => onSelectScreen('positions')}
          className="w-full bg-primary-container hover:bg-primary text-on-primary font-bold py-2 px-4 rounded-lg transition-colors text-sm shadow-sm flex items-center justify-center gap-2"
        >
          <span>New Trade</span>
        </button>
      </div>
    </aside>
  );
}
