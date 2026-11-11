import { useState, type ReactNode } from 'react';
import { ChevronDown, ChevronRight, CircleAlert } from 'lucide-react';
import { AccountChip, AccountRow } from '../auth/AccountChip';
import { signedMoney, toneOf } from '../lib/format';
import { useIstClock } from '../lib/hooks';
import { useNavHidden } from '../lib/layout';
import type { Health, HealthSet, Level, Mode, Role, StatusAnswer } from '../lib/types';
import { useTickFlash } from '../lib/useTickFlash';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { HealthDot } from '../ui/Health';
import { SCREENS, type ScreenId } from './screens';

// Figma "Status bar / Desktop" and "/ Phone". Always visible: Paper or LIVE,
// the health dots, the day's net P&L, write access and the kill switch —
// for the owner and the admin. A user or a visitor sees the clock, the
// market, the price feed's dot and their own sign-in: the owner's figures
// and controls are the owner's.

const DOTS: { key: keyof HealthSet; name: string }[] = [
  { key: 'engine', name: 'Engine' },
  { key: 'feed', name: 'Feed' },
  { key: 'kotak', name: 'Kotak' },
  { key: 'telegram', name: 'Telegram' },
  { key: 'upstox', name: 'Upstox' },
];

/** The dots `role` gets: everything for the owner's view, the feed alone otherwise. */
function dotsFor(role: Role) {
  return role === 'admin' ? DOTS : DOTS.filter((d) => d.key === 'feed');
}

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

function ModeChip({ mode }: { mode: Mode | undefined }) {
  if (mode === 'LIVE') return <Chip kind="live">LIVE</Chip>;
  return <Chip>{mode === 'PAPER' ? 'Paper' : '…'}</Chip>;
}

/** The bar's surface: quiet grey, or, while real money is on, the marigold
 * wash edge to edge with its 3 px ink rule on top. Paper has neither. */
function barLook(mode: Mode | undefined): string {
  return mode === 'LIVE' ? 'border-t-[3px] border-t-live-ink bg-live-subtle' : 'bg-inset';
}

/** A dot's level, or every dot down while the server can't be reached. */
function dotOf(status: StatusAnswer | null, unreachable: boolean, key: keyof HealthSet): Health {
  if (unreachable) return { level: 'down', label: "Can't reach the server" };
  return status?.health?.[key] ?? { level: 'off', label: 'Waiting for the first reading' };
}

export function DesktopStatusBar({ status, unreachable, role, onKillSwitch, search }: {
  status: StatusAnswer | null;
  unreachable: boolean;
  role: Role;
  onKillSwitch: () => void;
  /** The instrument search's well (`shell/Search.tsx`), while the search is on. */
  search?: ReactNode;
}) {
  const clock = useIstClock();
  // Paper and LIVE are different figures, not a tick.
  const flash = useTickFlash(status?.day_pnl, true, status?.mode);
  const ownerView = role === 'admin';
  const [navHidden, toggleNav] = useNavHidden();
  const navAction = navHidden ? 'Show the navigation' : 'Hide the navigation';
  return (
    <header className={`flex h-10 shrink-0 items-center gap-4 border-b border-line px-4 ${barLook(status?.mode)}`}>
      <div className="flex min-w-0 flex-1 items-center gap-3">
        {/* The mode chip also folds the navigation rail away, for room. */}
        <button
          type="button"
          onClick={toggleNav}
          aria-pressed={navHidden}
          aria-label={`${status?.mode === 'LIVE' ? 'LIVE' : 'Paper'} mode: ${navAction.toLowerCase()}`}
          title={navAction}
          className="shrink-0 rounded-4 outline-none transition-opacity duration-120 hover:opacity-80 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
        >
          <ModeChip mode={status?.mode} />
        </button>
        <span className="whitespace-nowrap num-small text-fg">{clock} IST</span>
        {/* Beside the search well the market phrase needs a wide window; below it, it would only truncate. */}
        <span className={`truncate type-caption text-fg-subtle ${search ? 'hidden min-[1400px]:inline' : ''}`}>{status?.market ?? ''}</span>
      </div>
      {/* The search well sits in the centre; the health dots move right of it. */}
      {search}
      <div className="flex items-center gap-4">
        {dotsFor(role).map((d) => {
          const h = dotOf(status, unreachable, d.key);
          return (
            <span key={d.key} title={`${d.name}: ${h.label}`} className="flex items-center gap-1.5 type-caption text-fg-muted">
              <HealthDot level={h.level} />
              {d.name}
            </span>
          );
        })}
      </div>
      <div className="flex flex-1 items-center justify-end gap-3">
        {role !== 'anon' && (
          <>
            <span className="type-label text-fg-muted">Day</span>
            <span key={flash.key} className={`-mx-1 whitespace-nowrap rounded-4 px-1 num-medium ${TONE_TEXT[toneOf(status?.day_pnl)]} ${flash.className}`}>
              {status ? signedMoney(status.day_pnl) : '—'}
            </span>
            <span aria-hidden className="h-5 w-px bg-line" />
          </>
        )}
        <AccountChip />
        {ownerView &&
          (status?.kill_switch ? (
            <span className="inline-flex h-7 items-center gap-1.5 rounded-6 border border-loss px-2.5 type-button text-loss">
              <CircleAlert size={14} /> Trading stopped
            </span>
          ) : (
            <Button size="sm" kind="alert" icon={<CircleAlert size={14} />} onClick={onKillSwitch} disabled={!status}>
              Kill switch
            </Button>
          ))}
      </div>
    </header>
  );
}

/** "All connected", "Kotak down", "2 down". */
function summary(dots: { name: string; level: Level }[], unreachable: boolean): string {
  if (unreachable) return "Can't reach the server";
  const down = dots.filter((d) => d.level === 'down');
  if (down.length === 0) return 'All connected';
  return down.length === 1 ? `${down[0].name} down` : `${down.length} down`;
}

export function PhoneStatusBar({ status, unreachable, role, onKillSwitch, more }: {
  status: StatusAnswer | null;
  unreachable: boolean;
  role: Role;
  onKillSwitch: () => void;
  /** The screens with no tab of their own (Settings, Admin) while the Search
   * tab has More's place: rows in the dropdown, over the account row. */
  more?: { screens: ScreenId[]; screen: ScreenId; onSelect: (s: ScreenId) => void };
}) {
  const [open, setOpen] = useState(false);
  // Paper and LIVE are different figures, not a tick.
  const flash = useTickFlash(status?.day_pnl, true, status?.mode);
  const ownerView = role === 'admin';
  const dots = dotsFor(role).map((d) => ({ ...d, ...dotOf(status, unreachable, d.key) }));
  const worst: Level = dots.some((d) => d.level === 'down') ? 'down' : dots.every((d) => d.level === 'off') ? 'off' : 'ok';
  const moreScreens = more ? SCREENS.filter((s) => more.screens.includes(s.id)) : [];
  return (
    <header className={`relative z-30 shrink-0 border-b border-line pt-safe ${barLook(status?.mode)}`}>
      <div className="flex h-11 items-center gap-2 px-4">
        <ModeChip mode={status?.mode} />
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
          className="flex min-w-0 items-center gap-1.5 rounded-6 px-1.5 py-1 type-label text-fg transition-colors duration-120 hover:bg-raised"
        >
          <HealthDot level={worst} />
          <span className="truncate">{summary(dots, unreachable)}</span>
          <ChevronDown size={14} className={`shrink-0 text-fg-muted transition-transform ${open ? 'rotate-180' : ''}`} />
        </button>
        <span className="flex-1" />
        {role !== 'anon' && (
          <span key={flash.key} className={`-mx-1 rounded-4 px-1 num-medium ${TONE_TEXT[toneOf(status?.day_pnl)]} ${flash.className}`}>
            {status ? signedMoney(status.day_pnl) : '—'}
          </span>
        )}
        {ownerView &&
          (status?.kill_switch ? (
            <span role="img" aria-label="Trading stopped" className="grid size-9 place-items-center rounded-8 border border-loss bg-loss-subtle text-loss">
              <CircleAlert size={18} />
            </span>
          ) : (
            <button
              type="button"
              aria-label="Kill switch"
              onClick={onKillSwitch}
              disabled={!status}
              className="grid size-9 place-items-center rounded-8 border border-loss text-loss transition-colors duration-120 hover:bg-loss-subtle disabled:opacity-50"
            >
              <CircleAlert size={18} />
            </button>
          ))}
      </div>
      {open && (
        <div className="absolute inset-x-0 top-full border-b border-line bg-panel px-4 py-2 animate-fade-in">
          {dots.map((d) => (
            <div key={d.key} className="flex h-10 items-center gap-2.5">
              <HealthDot level={d.level} />
              <span className="type-body-strong text-fg">{d.name}</span>
              <span className="ml-auto type-caption text-fg-subtle">{d.label}</span>
            </div>
          ))}
          {more && moreScreens.length > 0 && (
            <nav aria-label="More screens" className="mt-1 border-t border-line pt-1">
              {moreScreens.map(({ id, label, icon: Icon }) => {
                const selected = id === more.screen;
                return (
                  <button
                    key={id}
                    type="button"
                    aria-current={selected ? 'page' : undefined}
                    onClick={() => {
                      setOpen(false);
                      more.onSelect(id);
                    }}
                    className={`-mx-2 flex h-11 w-[calc(100%+16px)] items-center gap-2.5 rounded-8 px-2 text-left type-body-strong transition-colors duration-120 ${
                      selected ? 'bg-accent-subtle text-accent' : 'text-fg hover:bg-raised/40'
                    }`}
                  >
                    <Icon size={18} strokeWidth={1.75} className={selected ? 'text-accent' : 'text-fg-muted'} />
                    <span className="flex-1">{label}</span>
                    <ChevronRight size={16} className="text-fg-muted" />
                  </button>
                );
              })}
            </nav>
          )}
          <div className="mt-1">
            <AccountRow />
          </div>
        </div>
      )}
    </header>
  );
}
