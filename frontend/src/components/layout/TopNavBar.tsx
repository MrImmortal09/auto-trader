import { Bell, User } from 'lucide-react';
import type { ScreenId } from '../../types';

export function TopNavBar({
  activeScreen,
  onNewTrade
}: {
  activeScreen: ScreenId;
  serverBase: string;
  onNewTrade: () => void;
}) {
  const titles: Record<ScreenId, string> = {
    dashboard: 'Trading Dashboard',
    positions: 'Active Positions & Signals',
    analytics: 'Trade Analytics & Order Flow Deep Dive',
    portfolio: 'Portfolio Performance & Reports',
    settings: 'Configurations & System Settings',
  };

  return (
    <header className="h-14 sm:h-16 bg-surface border-b border-outline-variant flex items-center justify-between px-4 sm:px-8 shrink-0 z-20 pt-safe">
      <div className="flex items-center gap-2.5 sm:gap-6 min-w-0">
        <div className="flex items-center gap-2 md:hidden shrink-0">
          <div className="w-8 h-8 rounded-lg bg-primary-container flex items-center justify-center text-on-primary font-bold text-xs shadow-sm">
            AT
          </div>
        </div>
        <h1 className="text-base sm:text-lg font-bold tracking-tight text-on-surface truncate">{titles[activeScreen]}</h1>
        <div className="hidden md:flex items-center gap-4 text-xs font-semibold text-on-surface-variant">
          <span className="cursor-pointer hover:text-primary transition-colors">Portfolio</span>
          <span className="cursor-pointer hover:text-primary transition-colors">Watchlist</span>
          <span className="cursor-pointer hover:text-primary transition-colors">Alerts</span>
        </div>
      </div>
      <div className="flex items-center gap-2 sm:gap-4 shrink-0">
        <button className="hidden sm:flex w-8 h-8 rounded-full items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <Bell size={18} />
        </button>
        <button className="hidden sm:flex w-8 h-8 rounded-full items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <User size={18} />
        </button>
        <button
          onClick={onNewTrade}
          className="bg-primary text-on-primary px-3 sm:px-3.5 py-1.5 rounded-lg text-xs font-bold hover:bg-primary/90 transition-colors shadow-sm flex items-center gap-1"
        >
          <span className="sm:hidden">+ Trade</span>
          <span className="hidden sm:inline">Execute Order</span>
        </button>
      </div>
    </header>
  );
}
