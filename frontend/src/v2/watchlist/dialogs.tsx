import { useState, type ReactNode } from 'react';
import { plural } from '../lib/format';
import type { Watchlist } from '../lib/types';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';

// The Watchlist screen's dialogs: a name for a new or renamed list, and
// "Delete this list?". Each sends nothing itself: `onSubmit` is the
// screen's, which talks to the store and toasts a refusal; the dialog waits
// on it ("Saving…") and closes once it went through.

function Actions({ desktop, children }: { desktop: boolean; children: ReactNode }) {
  return <div className={desktop ? 'flex justify-end gap-2' : 'flex flex-col-reverse gap-3 [&>button]:w-full'}>{children}</div>;
}

/** "New watchlist" / "Rename 'Banks'": one text well, 1–`maxChars` characters. */
export function NameDialog({ desktop, title, current = '', maxChars, submitLabel, onSubmit, onClose }: {
  desktop: boolean;
  title: string;
  /** The name as it is (a rename); empty for a new list. */
  current?: string;
  maxChars: number;
  submitLabel: string;
  /** Resolves `true` once saved; the dialog then closes. */
  onSubmit: (name: string) => Promise<boolean>;
  onClose: () => void;
}) {
  const [value, setValue] = useState(current);
  const [sending, setSending] = useState(false);
  const name = value.trim();
  const valid = name.length > 0 && name.length <= maxChars;
  const unchanged = name === current.trim();
  const size = desktop ? 'md' : 'lg';

  async function submit() {
    if (!valid || unchanged || sending) return;
    setSending(true);
    const ok = await onSubmit(name);
    setSending(false);
    if (ok) onClose();
  }

  return (
    <Modal open onClose={onClose} label={title} locked={sending} width="lg:max-w-[420px]">
      <form
        className="flex flex-col gap-4 p-6"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <h2 className="type-title text-fg">{title}</h2>
        <div className="flex flex-col gap-1.5">
          <label htmlFor="watchlist-name" className="type-label text-fg-muted">
            Name
          </label>
          <input
            id="watchlist-name"
            data-autofocus
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onFocus={(e) => e.currentTarget.select()}
            maxLength={maxChars}
            placeholder="Banks, Scalps, Monday…"
            autoComplete="off"
            spellCheck={false}
            disabled={sending}
            className={`h-11 rounded-6 border bg-inset px-3 type-body text-fg transition-colors duration-120 placeholder:text-fg-subtle focus:border-accent disabled:opacity-50 ${
              value.length > 0 && !valid ? 'border-loss' : 'border-line'
            }`}
          />
          <span className={`type-caption tnum ${value.length > 0 && !valid ? 'text-loss' : 'text-fg-subtle'}`}>
            {name.length} of {maxChars} characters
          </span>
        </div>
        <Actions desktop={desktop}>
          <Button type="button" size={size} onClick={onClose} disabled={sending}>
            Cancel
          </Button>
          <Button type="submit" size={size} kind="accent" disabled={!valid || unchanged || sending}>
            {sending ? 'Saving…' : submitLabel}
          </Button>
        </Actions>
      </form>
    </Modal>
  );
}

/** "Delete 'Banks'?" — its instruments go with it. */
export function DeleteDialog({ desktop, list, onConfirm, onClose }: {
  desktop: boolean;
  list: Watchlist;
  /** Resolves `true` once deleted; the dialog then closes. */
  onConfirm: () => Promise<boolean>;
  onClose: () => void;
}) {
  const [sending, setSending] = useState(false);
  const size = desktop ? 'md' : 'lg';

  async function confirm() {
    setSending(true);
    const ok = await onConfirm();
    setSending(false);
    if (ok) onClose();
  }

  return (
    <Modal open onClose={onClose} label={`Delete ${list.name}?`} locked={sending} width="lg:max-w-[420px]">
      <div className="flex flex-col gap-4 p-6">
        <h2 className="type-title text-fg">Delete '{list.name}'?</h2>
        <p className="type-body text-fg-muted">
          {list.items.length === 0 ? 'It is empty.' : `Its ${plural(list.items.length, 'instrument')} go with it.`} Your other lists stay as they are.
        </p>
        <Actions desktop={desktop}>
          <Button data-autofocus size={size} onClick={onClose} disabled={sending}>
            Cancel
          </Button>
          <Button size={size} kind="loss" onClick={() => void confirm()} disabled={sending}>
            {sending ? 'Deleting…' : 'Delete'}
          </Button>
        </Actions>
      </div>
    </Modal>
  );
}
