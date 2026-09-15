import { Bell, User, AlertOctagon, ShieldCheck, Lock, Eye, KeyRound } from 'lucide-react';
import type { ScreenId } from '../../types';
import { useAuth } from '../../context/AuthContext';

export function TopNavBar({
  activeScreen,
  onNewTrade,
  onOpenKillSwitch,
  killSwitchActive = false,
}: {
  activeScreen: ScreenId;
  serverBase: string;
  onNewTrade: () => void;
  onOpenKillSwitch?: () => void;
  killSwitchActive?: boolean;
}) {
  const { hasWriteAccess, openUnlockModal, lock } = useAuth();

  const titles: Record<ScreenId, string> = {
    dashboard: 'Trading Dashboard',
    positions: 'Active Positions & Signals',
    analytics: 'Trade Analytics & Order Flow Deep Dive',
    market: 'Market Data — OI, PCR & VWAP Trend',
    portfolio: 'Portfolio Performance & Reports',
    settings: 'Configurations & System Settings',
  };

  const handleExecuteClick = () => {
    if (!hasWriteAccess) {
      openUnlockModal('Order execution requires Write Access');
      return;
    }
    onNewTrade();
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
      <div className="flex items-center gap-2 sm:gap-3 shrink-0">
        {/* Role-based status pill */}
        {hasWriteAccess ? (
          <div className="flex items-center gap-1.5 bg-emerald-500/10 border border-emerald-500/30 text-emerald-400 px-2.5 py-1 rounded-full text-xs font-semibold">
            <ShieldCheck size={14} className="text-emerald-400 shrink-0" />
            <span className="hidden sm:inline">Write Access</span>
            <button
              type="button"
              onClick={lock}
              title="Lock session (switch to Read-Only mode)"
              className="ml-1 text-emerald-400/70 hover:text-emerald-300 p-0.5 rounded transition-colors"
            >
              <Lock size={12} />
            </button>
          </div>
        ) : (
          <button
            type="button"
            onClick={() => openUnlockModal('Enter passkey to enable write operations and order execution')}
            title="Click to unlock write access with passkey"
            className="flex items-center gap-1.5 bg-amber-500/10 border border-amber-500/30 text-amber-400 hover:bg-amber-500/20 px-2.5 py-1 rounded-full text-xs font-semibold transition-colors"
          >
            <Eye size={13} className="text-amber-400 shrink-0" />
            <span>Read-Only</span>
            <span className="text-amber-400/50 font-normal">|</span>
            <KeyRound size={12} className="shrink-0" />
            <span className="underline decoration-dotted hidden sm:inline">Unlock</span>
          </button>
        )}

        {onOpenKillSwitch && (
          <button
            type="button"
            onClick={onOpenKillSwitch}
            className={
              killSwitchActive
                ? 'bg-red-500/20 text-red-400 border border-red-500/40 px-2.5 sm:px-3 py-1.5 rounded-lg text-xs font-bold hover:bg-red-500/30 transition-all shadow-sm flex items-center gap-1.5 animate-pulse'
                : 'bg-red-950/40 text-red-400 border border-red-800/40 hover:bg-red-900/40 hover:border-red-600/50 px-2.5 sm:px-3 py-1.5 rounded-lg text-xs font-bold transition-all shadow-sm flex items-center gap-1.5'
            }
            title={killSwitchActive ? 'Kill Switch is Active (Trading Halted)' : 'Emergency Kill Switch'}
          >
            <AlertOctagon size={14} className={killSwitchActive ? 'text-red-400 animate-bounce' : 'text-red-400'} />
            <span className="hidden sm:inline">{killSwitchActive ? 'Halted (Kill Switch)' : 'Kill Switch'}</span>
            <span className="sm:hidden">{killSwitchActive ? 'Halted' : 'Kill'}</span>
          </button>
        )}
        <button className="hidden sm:flex w-8 h-8 rounded-full items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <Bell size={18} />
        </button>
        <button className="hidden sm:flex w-8 h-8 rounded-full items-center justify-center text-on-surface-variant hover:bg-surface-container-high transition-colors">
          <User size={18} />
        </button>
        <button
          onClick={handleExecuteClick}
          className="bg-primary text-on-primary px-3 sm:px-3.5 py-1.5 rounded-lg text-xs font-bold hover:bg-primary/90 transition-colors shadow-sm flex items-center gap-1"
        >
          <span className="sm:hidden">+ Trade</span>
          <span className="hidden sm:inline">Execute Order</span>
        </button>
      </div>
    </header>
  );
}
