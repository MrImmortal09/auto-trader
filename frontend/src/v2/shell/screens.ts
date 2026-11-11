import { Activity, Gauge, LayoutDashboard, Settings, ShieldCheck, Star, Zap, type LucideIcon } from 'lucide-react';
import type { Role } from '../lib/types';

export type ScreenId = 'desk' | 'trade' | 'market' | 'watchlist' | 'system' | 'settings' | 'admin' | 'instrument';

/** Where the app is: a screen, and for `instrument` which one (`/v2/i/<key>`,
 * the catalog id: `nse_cm:2885`). */
export interface Place {
  screen: ScreenId;
  key?: string;
}

/** The screens with a place in the navigation, in the desktop rail's order. */
export const SCREENS: { id: ScreenId; label: string; icon: LucideIcon }[] = [
  { id: 'desk', label: 'Desk', icon: LayoutDashboard },
  { id: 'trade', label: 'Trade', icon: Zap },
  { id: 'market', label: 'Market', icon: Gauge },
  { id: 'watchlist', label: 'Watchlist', icon: Star },
  { id: 'system', label: 'System', icon: Activity },
  { id: 'settings', label: 'Settings', icon: Settings },
  { id: 'admin', label: 'Admin', icon: ShieldCheck },
];

/**
 * The phone tab bar's own tabs, in order; the rest sit behind More (or under
 * the account menu while the Search tab has More's place). With watchlists
 * on, a user's tabs are Watchlist · Trade · Desk (the owner's answer to the
 * plan's question 7); the admin's stay Desk · Trade · Market, Watchlist
 * reachable from the rail and the account menu.
 */
export function tabScreens(role: Role, watchlists: boolean): ScreenId[] {
  return role !== 'admin' && watchlists ? ['watchlist', 'trade', 'desk'] : ['desk', 'trade', 'market'];
}

/** The screens `role` may open: users get the public ones and a small
 * Settings; the admin gets everything. An instrument page is anyone's; the
 * Watchlist screen is anyone's while `watchlists` (the admin's switch) is on. */
export function visibleScreens(role: Role, watchlists: boolean): ScreenId[] {
  const all: ScreenId[] = role === 'admin' ? ['desk', 'trade', 'market', 'watchlist', 'system', 'settings', 'admin', 'instrument'] : ['desk', 'trade', 'market', 'watchlist', 'settings', 'instrument'];
  return watchlists ? all : all.filter((id) => id !== 'watchlist');
}

/** Screens with no button anywhere: they open from their address alone
 * (`/v2/system`, `/v2/i/<key>` from the search), for whoever may open them. */
const LINK_ONLY: ScreenId[] = ['system', 'instrument'];

/** The screens the navigation shows `role`. */
export function navScreens(role: Role, watchlists: boolean): ScreenId[] {
  return visibleScreens(role, watchlists).filter((id) => !LINK_ONLY.includes(id));
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
