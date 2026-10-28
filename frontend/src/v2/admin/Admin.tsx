import { useState } from 'react';
import { Trash2, UserPlus } from 'lucide-react';
import { formatPhone, normalizePhone } from '../auth/phone';
import { createUser, deleteUser, listUsers, patchUser } from '../lib/api';
import { hhmm, plural, shortDate } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { AdminUser } from '../lib/types';
import { Row, Section } from '../settings/parts';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Modal } from '../ui/Modal';
import { Toggle } from '../ui/Toggle';
import { useToast } from '../ui/toast';
import { PaperLine, PaperPositions, PaperTrades } from './PaperActivity';

const INPUT = 'rounded-6 border border-line bg-inset px-2.5 text-fg outline-none placeholder:text-fg-muted focus:border-line-strong disabled:opacity-50';

/** "Last signed in 26 Oct 15:44", or "Never signed in". */
function lastLogin(u: AdminUser): string {
  if (!u.last_login_at) return 'Never signed in';
  return `Last signed in ${shortDate(u.last_login_at.slice(0, 10))} ${hhmm(u.last_login_at)}`;
}

/**
 * Admin: who may sign in by phone — the admin adds numbers, switches them off
 * and removes them, as paper traders; the admin's own row stays — and what
 * every user holds and has traded on paper, read-only.
 */
export function Admin({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const users = usePoll(() => listUsers(serverBase), 15_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const [phone, setPhone] = useState('');
  const [name, setName] = useState('');
  const [adding, setAdding] = useState(false);
  const [busy, setBusy] = useState<Record<number, true>>({});
  const [removing, setRemoving] = useState<AdminUser | null>(null);
  const large = !desktop;
  const size = large ? 'lg' : 'md';

  async function act(id: number, run: () => Promise<unknown>) {
    setBusy((b) => ({ ...b, [id]: true }));
    try {
      await run();
      await users.refresh();
    } finally {
      setBusy((b) => {
        const next = { ...b };
        delete next[id];
        return next;
      });
    }
  }

  async function add() {
    const normalized = normalizePhone(phone);
    if (!normalized) {
      toast({ tone: 'loss', title: 'Enter a 10-digit Indian mobile number' });
      return;
    }
    setAdding(true);
    const sent = await guard('Adding a user needs the admin sign-in.', () => createUser(serverBase, normalized, name.trim() || null), `Couldn't add ${formatPhone(normalized)}`, 'admin');
    setAdding(false);
    if (sent?.ok) {
      toast({ tone: 'profit', title: `Added ${name.trim() || formatPhone(normalized)}`, body: 'They can sign in with a WhatsApp code now.' });
      setPhone('');
      setName('');
      await users.refresh();
    }
  }

  const setEnabled = (u: AdminUser, enabled: boolean) =>
    act(u.id, () => guard('Changing a user needs the admin sign-in.', () => patchUser(serverBase, u.id, { enabled }), `Couldn't ${enabled ? 'enable' : 'disable'} ${formatPhone(u.phone)}`, 'admin'));

  async function remove(u: AdminUser) {
    setRemoving(null);
    await act(u.id, async () => {
      const sent = await guard('Removing a user needs the admin sign-in.', () => deleteUser(serverBase, u.id), `Couldn't remove ${formatPhone(u.phone)}`, 'admin');
      if (sent?.ok) toast({ tone: 'neutral', title: `Removed ${u.name ?? formatPhone(u.phone)}` });
    });
  }

  const list = users.data ?? [];
  const rows =
    users.data == null ? (
      <Row title={users.error ? `Couldn't load the users: ${users.error}` : 'Loading…'} />
    ) : (
      list.map((u) => {
        const isAdmin = u.role === 'admin';
        return (
          <Row
            key={u.id}
            title={
              <span className="flex items-center gap-2">
                <span className="truncate">{u.name?.trim() || formatPhone(u.phone)}</span>
                {isAdmin && <Chip kind="live">Admin</Chip>}
                {!u.enabled && <Chip kind="loss">Disabled</Chip>}
              </span>
            }
            caption={
              <>
                <div>{`+91 ${formatPhone(u.phone)} · ${lastLogin(u)} · added ${shortDate(u.created_at.slice(0, 10))}`}</div>
                {u.paper && <PaperLine p={u.paper} />}
              </>
            }
          >
            <Toggle checked={u.enabled} disabled={isAdmin || !!busy[u.id]} label={`${u.enabled ? 'Disable' : 'Enable'} ${formatPhone(u.phone)}`} onChange={(on) => void setEnabled(u, on)} />
            {!isAdmin && (
              <Button
                size={size}
                kind="quiet"
                icon={<Trash2 size={14} />}
                aria-label={`Remove ${formatPhone(u.phone)}`}
                // The server refuses while they hold anything: disable them instead.
                title={u.paper?.open ? `Holds ${plural(u.paper.open, 'open paper position')}: disable them instead, or remove them once flat` : undefined}
                disabled={!!busy[u.id] || (u.paper?.open ?? 0) > 0}
                onClick={() => setRemoving(u)}
              />
            )}
          </Row>
        );
      })
    );

  const addForm = (
    <form
      className={`flex gap-2 border-t border-line p-4 ${large ? 'flex-col' : 'items-center'}`}
      onSubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      <span className="relative flex items-center">
        <span className="pointer-events-none absolute left-2.5 type-caption text-fg-muted">+91</span>
        <input
          type="tel"
          inputMode="numeric"
          aria-label="Mobile number"
          placeholder="98765 43210"
          maxLength={14}
          value={phone}
          disabled={adding}
          onChange={(e) => setPhone(e.target.value.replace(/[^\d\s+-]/g, ''))}
          className={`${INPUT} ${large ? 'h-11 w-full' : 'h-8 w-[160px]'} pl-9 num-small tracking-[0.06em]`}
        />
      </span>
      <input type="text" aria-label="Name" placeholder="Name (optional)" maxLength={60} value={name} disabled={adding} onChange={(e) => setName(e.target.value)} className={`${INPUT} ${large ? 'h-11 w-full' : 'h-8 flex-1'} type-body`} />
      <Button size={size} kind="profit" type="submit" icon={<UserPlus size={14} />} disabled={adding || phone.replace(/\D/g, '').length < 10}>
        {adding ? 'Adding…' : 'Add'}
      </Button>
    </form>
  );

  return (
    <div className={desktop ? 'px-10 pt-6 pb-24' : 'p-4'}>
      <div className={`flex flex-col gap-8 ${desktop ? 'max-w-[760px]' : ''}`}>
        <Section
          title="Users"
          lead="Who may sign in with a phone number, as a paper trader. The admin is the server's own number (ADMIN_PHONE) and nobody else. A disabled number is signed out at once."
        >
          {rows}
          {addForm}
        </Section>
        <PaperPositions serverBase={serverBase} users={list} />
        <PaperTrades serverBase={serverBase} users={list} desktop={desktop} />
      </div>
      {removing && (
        <Modal open onClose={() => setRemoving(null)} label="Remove user" width="lg:max-w-[400px]">
          <div className="flex flex-col gap-4 p-6">
            <h2 className="type-title text-fg">Remove {removing.name?.trim() || formatPhone(removing.phone)}?</h2>
            <p className="type-body text-fg-muted">
              +91 {formatPhone(removing.phone)} is taken off the allow-list and signed out at once, and their paper portfolio and its trades are deleted. Add the number again to let them back in, starting fresh.
            </p>
            <div className="flex justify-end gap-2">
              <Button size="md" onClick={() => setRemoving(null)}>
                Cancel
              </Button>
              <Button size="md" kind="loss" icon={<Trash2 size={14} />} onClick={() => void remove(removing)}>
                Remove
              </Button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
