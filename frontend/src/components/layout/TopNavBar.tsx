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
    <header className="h-16 bg-surface border-b border-outline-variant flex items-center justify-between px-8 shrink-0 z-20">
      <div className="flex items-center gap-6">
        <h1 className="text-lg font-bold tracking-tight text-on-surface">{titles[activeScreen]}</h1>
        <div className="hidden md:flex items-center gap-4 text-xs font-semibold text-on-surface-variant">
          <span className="cursor-pointer hover:text-primary transition-colors">Portfolio</span>
          <span className="cursor-pointer hover:text-primary transition-colors">Watchlist</span>
          <span className="cursor-pointer hover:text-primary transition-colors">Alerts</span>
        </div>
      </div>
      <div className="flex items-center gap-4">
        <button className="w-8 h-8 rounded-full flex items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <Bell size={18} />
        </button>
        <button className="w-8 h-8 rounded-full flex items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <User size={18} />
        </button>
        <button
          onClick={onNewTrade}
          className="bg-primary text-on-primary px-3.5 py-1.5 rounded-lg text-xs font-bold hover:bg-primary/90 transition-colors shadow-sm"
        >
          Execute Order
        </button>
      </div>
    </header>
  );
}
