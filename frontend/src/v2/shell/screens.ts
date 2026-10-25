import { Activity, Gauge, LayoutDashboard, Settings, Zap, type LucideIcon } from 'lucide-react';

export type ScreenId = 'desk' | 'trade' | 'market' | 'system' | 'settings';

export const SCREENS: { id: ScreenId; label: string; icon: LucideIcon }[] = [
  { id: 'desk', label: 'Desk', icon: LayoutDashboard },
  { id: 'trade', label: 'Trade', icon: Zap },
  { id: 'market', label: 'Market', icon: Gauge },
  { id: 'system', label: 'System', icon: Activity },
  { id: 'settings', label: 'Settings', icon: Settings },
];

/** The screen named in `location.hash` (`/v2/#trade`), or Desk. `#scalp`,
 * the Trade screen's old name, still opens it. */
export function screenFromHash(): ScreenId {
  const id = window.location.hash.replace(/^#/, '').split('/')[0];
  if (id === 'scalp') return 'trade';
  return SCREENS.some((s) => s.id === id) ? (id as ScreenId) : 'desk';
}
