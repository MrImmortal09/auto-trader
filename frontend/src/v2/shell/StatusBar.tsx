import { useState } from 'react';
import { ChevronDown, CircleAlert, Lock, LockOpen } from 'lucide-react';
import { useAuth } from '../../context/AuthContext';
import { AccountChip, AccountRow } from '../auth/AccountChip';
import { signedMoney, toneOf } from '../lib/format';
import { useIstClock } from '../lib/hooks';
import type { Health, HealthSet, Level, Mode, Role, StatusAnswer } from '../lib/types';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { HealthDot } from '../ui/Health';

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
  return role === 'owner' || role === 'admin' ? DOTS : DOTS.filter((d) => d.key === 'feed');
}

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

function ModeChip({ mode }: { mode: Mode | undefined }) {
  if (mode === 'LIVE') return <Chip kind="live">LIVE</Chip>;
  return <Chip>{mode === 'PAPER' ? 'Paper' : '…'}</Chip>;
}

/** A dot's level, or every dot down while the server can't be reached. */
function dotOf(status: StatusAnswer | null, unreachable: boolean, key: keyof HealthSet): Health {
  if (unreachable) return { level: 'down', label: "Can't reach the server" };
  return status?.health?.[key] ?? { level: 'off', label: 'Waiting for the first reading' };
}

function WriteAccess() {
  const { hasWriteAccess, openUnlockModal, lock } = useAuth();
  return hasWriteAccess ? (
    <Button size="sm" kind="ghost" className="border border-line" icon={<LockOpen size={14} />} onClick={lock} title="Lock: switch to read-only">
      Write access
    </Button>
  ) : (
    <Button size="sm" kind="ghost" className="border border-line" icon={<Lock size={14} />} onClick={() => openUnlockModal()} title="Enter the passkey to place orders">
      Read only
    </Button>
  );
}

export function DesktopStatusBar({ status, unreachable, role, onKillSwitch }: {
  status: StatusAnswer | null;
  unreachable: boolean;
  role: Role;
  onKillSwitch: () => void;
}) {
  const clock = useIstClock();
  const ownerView = role === 'owner' || role === 'admin';
  return (
    <header className="flex h-10 shrink-0 items-center gap-4 border-b border-line bg-inset px-4">
      <div className="flex min-w-0 flex-1 items-center gap-3">
        <ModeChip mode={status?.mode} />
        <span className="num-small text-fg">{clock} IST</span>
        <span className="truncate type-caption text-fg-muted">{status?.market ?? ''}</span>
      </div>
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
        {ownerView && (
          <>
            <span className="type-label text-fg-muted">Day</span>
            <span className={`num-medium ${TONE_TEXT[toneOf(status?.day_pnl)]}`}>{status ? signedMoney(status.day_pnl) : '—'}</span>
            <span aria-hidden className="h-5 w-px bg-line" />
          </>
        )}
        <AccountChip />
        {ownerView && <WriteAccess />}
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

export function PhoneStatusBar({ status, unreachable, role, onKillSwitch }: {
  status: StatusAnswer | null;
  unreachable: boolean;
  role: Role;
  onKillSwitch: () => void;
}) {
  const [open, setOpen] = useState(false);
  const { hasWriteAccess, openUnlockModal, lock } = useAuth();
  const ownerView = role === 'owner' || role === 'admin';
  const dots = dotsFor(role).map((d) => ({ ...d, ...dotOf(status, unreachable, d.key) }));
  const worst: Level = dots.some((d) => d.level === 'down') ? 'down' : dots.every((d) => d.level === 'off') ? 'off' : 'ok';
  return (
    <header className="relative z-30 shrink-0 border-b border-line bg-inset pt-safe">
      <div className="flex h-11 items-center gap-2 px-4">
        <ModeChip mode={status?.mode} />
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
          className="flex min-w-0 items-center gap-1.5 rounded-6 px-1.5 py-1 type-label text-fg hover:bg-raised"
        >
          <HealthDot level={worst} />
          <span className="truncate">{summary(dots, unreachable)}</span>
          <ChevronDown size={14} className={`shrink-0 text-fg-muted transition-transform ${open ? 'rotate-180' : ''}`} />
        </button>
        <span className="flex-1" />
        {ownerView && <span className={`num-medium ${TONE_TEXT[toneOf(status?.day_pnl)]}`}>{status ? signedMoney(status.day_pnl) : '—'}</span>}
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
              className="grid size-9 place-items-center rounded-8 border border-loss text-loss hover:bg-loss-subtle disabled:opacity-50"
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
              <span className="ml-auto type-caption text-fg-muted">{d.label}</span>
            </div>
          ))}
          <div className="mt-1">
            <AccountRow />
          </div>
          {ownerView && (
            <div className="flex h-12 items-center gap-2.5 border-t border-line">
              {hasWriteAccess ? <LockOpen size={16} className="text-fg-muted" /> : <Lock size={16} className="text-fg-muted" />}
              <span className="type-body-strong text-fg">{hasWriteAccess ? 'Write access' : 'Read only'}</span>
              <Button size="sm" className="ml-auto" onClick={() => (hasWriteAccess ? lock() : openUnlockModal())}>
                {hasWriteAccess ? 'Lock' : 'Unlock'}
              </Button>
            </div>
          )}
        </div>
      )}
    </header>
  );
}
