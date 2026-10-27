import { useState } from 'react';
import { LogIn, LogOut, UserRound } from 'lucide-react';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Popover } from '../ui/Popover';
import { useIdentity } from './identity';
import { displayName, formatPhone } from './phone';

/**
 * Desktop status bar: who is signed in by phone, with Sign out behind it —
 * or Sign in. The owner's passkey has its own lock button beside this.
 */
export function AccountChip() {
  const { role, user, openSignIn, signOut } = useIdentity();
  const [open, setOpen] = useState(false);
  if (!user) {
    if (role === 'owner') return null;
    return (
      <Button size="sm" kind="profit" icon={<LogIn size={14} />} onClick={() => openSignIn()}>
        Sign in
      </Button>
    );
  }
  return (
    <span className="relative">
      <Button size="sm" kind="ghost" className="border border-line" icon={<UserRound size={14} />} onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-label={`Account: ${displayName(user)}`}>
        <span className="max-w-[140px] truncate">{displayName(user)}</span>
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} align="right">
        <div className="flex flex-col gap-3 p-1">
          <div className="flex flex-col gap-1">
            <span className="type-body-strong text-fg">{displayName(user)}</span>
            <span className="type-caption text-fg-muted tnum">+91 {formatPhone(user.phone)}</span>
            <Chip kind={user.role === 'admin' ? 'live' : 'neutral'} className="self-start">
              {user.role === 'admin' ? 'Admin' : 'Paper trader'}
            </Chip>
          </div>
          <Button
            size="md"
            icon={<LogOut size={14} />}
            onClick={() => {
              setOpen(false);
              signOut();
            }}
          >
            Sign out
          </Button>
        </div>
      </Popover>
    </span>
  );
}

/** Phone status bar's dropdown row: the same, as a row with one button. */
export function AccountRow() {
  const { role, user, openSignIn, signOut } = useIdentity();
  if (!user && role === 'owner') return null;
  return (
    <div className="flex h-12 items-center gap-2.5 border-t border-line">
      <UserRound size={16} className="text-fg-muted" />
      <span className="min-w-0 flex-1 truncate type-body-strong text-fg">{user ? displayName(user) : 'Not signed in'}</span>
      {user ? (
        <Button size="sm" onClick={signOut}>
          Sign out
        </Button>
      ) : (
        <Button size="sm" kind="profit" onClick={() => openSignIn()}>
          Sign in
        </Button>
      )}
    </div>
  );
}
