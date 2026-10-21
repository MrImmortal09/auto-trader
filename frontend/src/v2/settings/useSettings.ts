import { useRef, useState } from 'react';
import type { TradingConfig } from '../../types';
import { getSettings, saveSettings, type Sent } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import { useGuard } from '../shell/useGuard';
import { useToast } from '../ui/toast';

/** What a save changes, worked out from the server's latest config. */
export type Change = (latest: TradingConfig) => Partial<TradingConfig>;

export interface SettingsState {
  /** The saved config, with saves still in flight already applied. */
  cfg: TradingConfig | null;
  error: string | null;
  /** Save `change` and say so in a toast: `what` names it ("Target 1: 50%"). */
  save: (change: Change, what: string) => Promise<boolean>;
}

/**
 * The trading config, and saving it the safe way: every save reads the
 * server's latest config first, changes only its own fields and sends the
 * whole config back (`POST /api/settings` replaces it all, including fields
 * v2 never shows). Saves run one after another, so two quick changes never
 * overwrite each other.
 */
export function useSettings(serverBase: string): SettingsState {
  const polled = usePoll(() => getSettings(serverBase), 30_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  // Changes on their way, shown at once.
  const [pending, setPending] = useState<Partial<TradingConfig>>({});
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  const inFlight = useRef(0);

  const cfg = polled.data ? { ...polled.data, ...pending } : null;

  async function send(change: Change): Promise<Sent> {
    let latest: TradingConfig;
    try {
      latest = await getSettings(serverBase);
    } catch {
      return { ok: false, status: 0, error: "Couldn't read the current settings, so nothing was saved." };
    }
    return saveSettings(serverBase, { ...latest, ...change(latest) });
  }

  function save(change: Change, what: string): Promise<boolean> {
    if (cfg) setPending((p) => ({ ...p, ...change(cfg) }));
    inFlight.current += 1;
    const run = queue.current.then(async () => {
      const sent = await guard('Changing settings needs write access.', () => send(change), "Couldn't save");
      if (sent?.ok) toast({ tone: 'neutral', title: 'Saved', body: what });
      return sent?.ok === true;
    });
    queue.current = run.catch(() => false);
    return run.finally(async () => {
      inFlight.current -= 1;
      if (inFlight.current === 0) {
        // Show what the server stored: it clamps out-of-range values.
        await polled.refresh();
        setPending({});
      }
    });
  }

  return { cfg, error: polled.error, save };
}
