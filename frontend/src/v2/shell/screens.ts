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

/** The screens `role` may open: users get the public ones and a small
 * Settings; the admin gets everything. */
export function visibleScreens(role: Role): ScreenId[] {
  return role === 'admin' ? ['desk', 'trade', 'market', 'system', 'settings', 'admin'] : ['desk', 'trade', 'market', 'settings'];
}

/** Screens with no button anywhere: they open from their address alone
 * (`/v2/system`), for whoever may open them. */
const LINK_ONLY: ScreenId[] = ['system'];

/** The screens the navigation shows `role`. */
export function navScreens(role: Role): ScreenId[] {
  return visibleScreens(role).filter((id) => !LINK_ONLY.includes(id));
}

/** v2 lives under `/v2/`, one path per screen: `/v2/trade`. */
export const screenPath = (id: ScreenId) => `/v2/${id}`;

/** The screen the address names — `/v2/trade`, or an old `/#trade` link —
 * or Desk. `scalp`, the Trade screen's old name, still opens it. */
export function screenFromLocation(): ScreenId {
  const fromPath = window.location.pathname.match(/^\/v2\/([^/]+)/)?.[1];
  const id = fromPath ?? window.location.hash.replace(/^#/, '').split('/')[0];
  if (id === 'scalp') return 'trade';
  return SCREENS.some((s) => s.id === id) ? (id as ScreenId) : 'desk';
}
