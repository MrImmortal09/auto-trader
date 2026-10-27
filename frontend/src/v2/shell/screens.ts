import { Activity, Gauge, LayoutDashboard, Settings, ShieldCheck, Zap, type LucideIcon } from 'lucide-react';
import type { Role } from '../lib/types';

export type ScreenId = 'desk' | 'trade' | 'market' | 'system' | 'settings' | 'admin';

export const SCREENS: { id: ScreenId; label: string; icon: LucideIcon }[] = [
  { id: 'desk', label: 'Desk', icon: LayoutDashboard },
  { id: 'trade', label: 'Trade', icon: Zap },
  { id: 'market', label: 'Market', icon: Gauge },
  { id: 'system', label: 'System', icon: Activity },
  { id: 'settings', label: 'Settings', icon: Settings },
  { id: 'admin', label: 'Admin', icon: ShieldCheck },
];

/** The phone tab bar's own tabs; the rest sit behind More. */
export const TAB_SCREENS: ScreenId[] = ['desk', 'trade', 'market'];

/** The screens `role` may open: visitors and users get the public ones and
 * a small Settings; the owner and the admin get everything. */
export function visibleScreens(role: Role): ScreenId[] {
  return role === 'owner' || role === 'admin' ? ['desk', 'trade', 'market', 'system', 'settings', 'admin'] : ['desk', 'trade', 'market', 'settings'];
}

/** The screen named in `location.hash` (`/v2/#trade`), or Desk. `#scalp`,
 * the Trade screen's old name, still opens it. */
export function screenFromHash(): ScreenId {
  const id = window.location.hash.replace(/^#/, '').split('/')[0];
  if (id === 'scalp') return 'trade';
  return SCREENS.some((s) => s.id === id) ? (id as ScreenId) : 'desk';
}
