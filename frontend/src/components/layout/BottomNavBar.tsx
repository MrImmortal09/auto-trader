import {
  BarChart2,
  LayoutDashboard,
  Settings,
  TrendingUp,
  Wallet,
} from 'lucide-react';
import type { ScreenId } from '../../types';

export function BottomNavBar({
  activeScreen,
  onSelectScreen,
}: {
  activeScreen: ScreenId;
  onSelectScreen: (screen: ScreenId) => void;
}) {
  const itemClass = (active: boolean) =>
    `flex flex-col items-center justify-center py-1.5 px-2 rounded-xl transition-all select-none cursor-pointer ${
      active
        ? 'text-primary font-bold'
        : 'text-on-surface-variant hover:text-on-surface'
    }`;

  return (
    <nav className="md:hidden fixed bottom-0 left-0 right-0 bg-surface/90 backdrop-blur-md border-t border-outline-variant/60 flex items-center justify-around px-2 pt-2 pb-safe z-50 shadow-lg">
      <div
        onClick={() => onSelectScreen('dashboard')}
        className={itemClass(activeScreen === 'dashboard')}
      >
        <LayoutDashboard size={20} className={activeScreen === 'dashboard' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Dashboard</span>
      </div>
      <div
        onClick={() => onSelectScreen('positions')}
        className={itemClass(activeScreen === 'positions')}
      >
        <Wallet size={20} className={activeScreen === 'positions' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Positions</span>
      </div>
      <div
        onClick={() => onSelectScreen('analytics')}
        className={itemClass(activeScreen === 'analytics')}
      >
        <BarChart2 size={20} className={activeScreen === 'analytics' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Analytics</span>
      </div>
      <div
        onClick={() => onSelectScreen('portfolio')}
        className={itemClass(activeScreen === 'portfolio')}
      >
        <TrendingUp size={20} className={activeScreen === 'portfolio' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Portfolio</span>
      </div>
      <div
        onClick={() => onSelectScreen('settings')}
        className={itemClass(activeScreen === 'settings')}
      >
        <Settings size={20} className={activeScreen === 'settings' ? 'scale-110 transition-transform' : ''} />
        <span className="text-[10px] mt-1 font-medium">Settings</span>
      </div>
    </nav>
  );
}
