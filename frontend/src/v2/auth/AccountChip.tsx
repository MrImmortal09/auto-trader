import { useState } from 'react';
import { LogIn, LogOut, UserRound } from 'lucide-react';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Popover } from '../ui/Popover';
import { useIdentity } from './identity';
import type { SessionUser } from '../lib/types';
import { displayName, formatPhone } from './phone';

const roleLabel = (role: 'admin' | 'user') => (role === 'admin' ? 'Admin' : 'Paper trader');

/** The phone sign-in's name: the server's record, else (not yet confirmed) its role. */
function phoneName(user: SessionUser | null, phoneRole: 'admin' | 'user'): string {
  return user ? displayName(user) : roleLabel(phoneRole);
}

/**
 * Desktop status bar: who is signed in, with Sign out behind it — or Sign in.
 */
export function AccountChip() {
  const { user, phoneRole, openSignIn, signOut } = useIdentity();
  const [open, setOpen] = useState(false);
  if (!phoneRole) {
    return (
      <Button size="sm" kind="profit" icon={<LogIn size={14} />} onClick={() => openSignIn()}>
        Sign in
      </Button>
    );
  }
  const name = phoneName(user, phoneRole);
  return (
    <span className="relative">
      <Button size="sm" kind="ghost" className="border border-line" icon={<UserRound size={14} />} onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-label={`Account: ${name}`}>
        <span className="max-w-[140px] truncate">{name}</span>
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} align="right">
        <div className="flex flex-col gap-3 p-1">
          <div className="flex flex-col gap-1">
            <span className="type-body-strong text-fg">{name}</span>
            {user && <span className="type-caption text-fg-muted tnum">+91 {formatPhone(user.phone)}</span>}
            <Chip kind={phoneRole === 'admin' ? 'live' : 'neutral'} className="self-start">
              {roleLabel(phoneRole)}
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
  const { user, phoneRole, openSignIn, signOut } = useIdentity();
  return (
    <div className="flex h-12 items-center gap-2.5 border-t border-line">
      <UserRound size={16} className="text-fg-muted" />
      <span className="min-w-0 flex-1 truncate type-body-strong text-fg">{phoneRole ? phoneName(user, phoneRole) : 'Not signed in'}</span>
      {phoneRole ? (
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
