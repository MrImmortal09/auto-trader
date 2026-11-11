import { Activity, Gauge, LayoutDashboard, ScrollText, Settings, ShieldCheck, Zap, type LucideIcon } from 'lucide-react';
import type { Role } from '../lib/types';

export type ScreenId = 'desk' | 'trade' | 'market' | 'logs' | 'system' | 'settings' | 'admin' | 'instrument';

/** Where the app is: a screen, and for `instrument` which one (`/v2/i/<key>`,
 * the catalog id: `nse_cm:2885`). */
export interface Place {
  screen: ScreenId;
  key?: string;
}

/** The screens with a place in the navigation. */
export const SCREENS: { id: ScreenId; label: string; icon: LucideIcon }[] = [
  { id: 'desk', label: 'Desk', icon: LayoutDashboard },
  { id: 'trade', label: 'Trade', icon: Zap },
  { id: 'market', label: 'Market', icon: Gauge },
  { id: 'logs', label: 'Logs', icon: ScrollText },
  { id: 'system', label: 'System', icon: Activity },
  { id: 'settings', label: 'Settings', icon: Settings },
  { id: 'admin', label: 'Admin', icon: ShieldCheck },
];

/** The phone tab bar's own tabs; the rest sit behind More. */
export const TAB_SCREENS: ScreenId[] = ['desk', 'trade', 'market'];

/** The screens `role` may open: users get the public ones and a small
 * Settings; the admin gets everything, the engine log (Logs) included. An
 * instrument page is anyone's. */
export function visibleScreens(role: Role): ScreenId[] {
  return role === 'admin'
    ? ['desk', 'trade', 'market', 'logs', 'system', 'settings', 'admin', 'instrument']
    : ['desk', 'trade', 'market', 'settings', 'instrument'];
}

/** Screens with no button anywhere: they open from their address alone
 * (`/v2/system`, `/v2/i/<key>` from the search), for whoever may open them. */
const LINK_ONLY: ScreenId[] = ['system', 'instrument'];

/** The screens the navigation shows `role`. */
export function navScreens(role: Role): ScreenId[] {
  return visibleScreens(role).filter((id) => !LINK_ONLY.includes(id));
}

/** v2 lives under `/v2/`, one path per screen: `/v2/trade`; an instrument
 * page is `/v2/i/<key>`, the key URL-encoded but for its colon
 * (`/v2/i/nse_cm:2885`, `/v2/i/nse_cm:Nifty%2050`). */
export function screenPath(place: Place | ScreenId): string {
  const p: Place = typeof place === 'string' ? { screen: place } : place;
  if (p.screen === 'instrument') return p.key ? `/v2/i/${encodeURIComponent(p.key).replace(/%3A/gi, ':')}` : '/v2/desk';
  return `/v2/${p.screen}`;
}

/** The place the address names — `/v2/trade`, `/v2/i/nse_cm:2885`, or an old
 * `/#trade` link — or Desk. `scalp`, the Trade screen's old name, still
 * opens it. */
export function screenFromLocation(): Place {
  const path = window.location.pathname;
  const instrument = path.match(/^\/v2\/i\/([^/]+)/)?.[1];
  if (instrument) {
    try {
      const key = decodeURIComponent(instrument);
      if (key) return { screen: 'instrument', key };
    } catch {
      // A malformed address: Desk.
    }
  }
  const fromPath = path.match(/^\/v2\/([^/]+)/)?.[1];
  const id = fromPath ?? window.location.hash.replace(/^#/, '').split('/')[0];
  if (id === 'scalp') return { screen: 'trade' };
  return { screen: SCREENS.some((s) => s.id === id) ? (id as ScreenId) : 'desk' };
}
