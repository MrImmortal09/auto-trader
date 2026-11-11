import { useState, type ReactNode } from 'react';
import { useIsDesktop } from '../lib/hooks';
import type { InstrumentRow, Watchlist } from '../lib/types';
import { currentWatchlists, loadWatchlists, useWatchlists, watchlistActions } from '../lib/useWatchlists';
import { rowTitle } from '../shell/searchModel';
import { Button } from '../ui/Button';
import { CheckboxRow } from '../ui/CheckboxRow';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';

// The "+" on a search result or an instrument page. With one list it goes
// straight in (a toast says so); with several, a small picker — every list
// as a checkbox row, ticked where it already holds the instrument — so a
// tap adds or takes out. One of these lives in the app shell, so the
// picker is one dialog over whatever screen asked.

/** `record` less `key`. */
function without<T>(record: Record<number, T>, key: number): Record<number, T> {
  const next = { ...record };
  delete next[key];
  return next;
}

export function useAddToWatchlist(serverBase: string, enabled: boolean): { add: (row: InstrumentRow) => void; picker: ReactNode } {
  const data = useWatchlists(serverBase, { enabled });
  const toast = useToast();
  const desktop = useIsDesktop();
  const [picking, setPicking] = useState<InstrumentRow | null>(null);
  // Lists with a change in flight: their rows wait for the answer.
  const [busy, setBusy] = useState<Record<number, true>>({});

  /** Put `row` in `list`, or take it out — and say how it went. */
  async function toggle(list: Watchlist, row: InstrumentRow, holds: boolean, quiet: boolean): Promise<boolean> {
    const title = rowTitle(row);
    setBusy((b) => ({ ...b, [list.id]: true }));
    const sent = holds ? await watchlistActions.removeItem(list.id, row.ws_key) : await watchlistActions.addItem(list.id, row.ws_key);
    setBusy((b) => without(b, list.id));
    if (!sent.ok) {
      // A 401 already dropped the sign-in, which brings back the sign-in page (see lib/api).
      if (sent.status !== 401) toast({ tone: 'loss', title: holds ? `Couldn't remove ${title}` : `Couldn't add ${title}`, body: sent.error ?? 'The server refused it.' });
      return false;
    }
    if (!quiet) toast({ tone: 'neutral', title: holds ? `${title} removed from ${list.name}` : `${title} added to ${list.name}` });
    return true;
  }

  async function add(row: InstrumentRow) {
    if (!currentWatchlists()) await loadWatchlists();
    const answer = currentWatchlists();
    if (!answer) {
      toast({ tone: 'loss', title: "Couldn't load your watchlists", body: 'Try again in a moment.' });
      return;
    }
    if (answer.watchlists.length > 1) {
      setPicking(row);
      return;
    }
    const list = answer.watchlists[0];
    if (!list) {
      toast({ tone: 'loss', title: "Couldn't add it", body: 'You have no watchlist yet — open the Watchlist screen first.' });
      return;
    }
    if (list.items.some((i) => i.key === row.ws_key)) {
      toast({ tone: 'neutral', title: `${rowTitle(row)} is already in ${list.name}` });
      return;
    }
    await toggle(list, row, false, false);
  }

  const lists = data.lists;
  const max = data.answer?.max_items ?? 50;
  const picker = picking && (
    <Modal open onClose={() => setPicking(null)} label={`Add ${rowTitle(picking)} to a watchlist`} width="lg:max-w-[400px]">
      <div className="flex flex-col gap-4 p-6">
        <div>
          <h2 className="type-title text-fg">Add {rowTitle(picking)} to…</h2>
          <p className="mt-0.5 type-caption text-fg-subtle">Tick a list to add it there; untick to take it out.</p>
        </div>
        <div className="-mx-2 flex flex-col">
          {lists.map((list) => {
            const holds = list.items.some((i) => i.key === picking.ws_key);
            const full = !holds && list.items.length >= max;
            return (
              <CheckboxRow
                key={list.id}
                checked={holds}
                disabled={busy[list.id] === true || full}
                title={list.name}
                note={busy[list.id] ? (holds ? 'Removing…' : 'Adding…') : full ? `Full — ${max} instruments; remove one first` : `${list.items.length} of ${max}`}
                onChange={() => void toggle(list, picking, holds, true)}
              />
            );
          })}
        </div>
        <div className={desktop ? 'flex justify-end' : 'flex flex-col'}>
          <Button data-autofocus size={desktop ? 'md' : 'lg'} onClick={() => setPicking(null)}>
            Done
          </Button>
        </div>
      </div>
    </Modal>
  );

  return { add: (row) => void add(row), picker };
}
