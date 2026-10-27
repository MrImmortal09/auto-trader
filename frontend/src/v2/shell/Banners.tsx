import { useState } from 'react';
import { getKotak, kotakAutoLogin } from '../lib/api';
import { indexName, todayIst } from '../lib/format';
import type { StatusAnswer } from '../lib/types';
import { Banner } from '../ui/Banner';
import { useToast } from '../ui/toast';
import { useGuard } from './useGuard';

const DISMISSED = 'v2.expiryDismissed';

/** Prices older than this, in market hours, and the feed is called stale. */
const STALE_SECS = 10;

function dismissedOn(): string | null {
  try {
    return localStorage.getItem(DISMISSED);
  } catch {
    return null;
  }
}

/** "Nifty weekly", "Nifty weekly and Bank Nifty monthly". */
function expiringLine(e: StatusAnswer['expiring']): string {
  const names = e.map((x) => `${indexName(x.index)} ${x.series}`);
  return names.length > 1 ? `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}` : names[0];
}

/**
 * The banners under the status bar, besides Trading stopped: Kotak's session
 * gone in market hours, a stale price feed, and an expiry day with options
 * still held. The Kotak one is the owner's and the admin's to act on; users
 * and visitors get the stale-feed banner alone.
 */
export function StatusBanners({ serverBase, status, canReadOwner }: { serverBase: string; status: StatusAnswer | null; canReadOwner: boolean }) {
  const guard = useGuard();
  const toast = useToast();
  const [connecting, setConnecting] = useState(false);
  const [dismissed, setDismissed] = useState(dismissedOn);
  if (!status) return null;
  const open = status.market_open;
  const kotak = status.health?.kotak;
  const feed = status.health?.feed;
  const live = status.mode === 'LIVE';
  const today = todayIst();

  async function reconnect() {
    setConnecting(true);
    try {
      const k = await getKotak(serverBase).catch(() => null);
      // Without saved credentials it's the login form, in the old design.
      if (!k?.has_env_credentials) {
        window.location.href = '/v1/settings';
        return;
      }
      const sent = await guard('Logging in to Kotak needs write access.', () => kotakAutoLogin(serverBase), "Couldn't log in to Kotak");
      if (sent?.ok) toast({ tone: 'profit', title: 'Kotak connected' });
    } finally {
      setConnecting(false);
    }
  }

  const banners = [];
  if (canReadOwner && open && kotak?.level === 'down') {
    banners.push(
      <Banner
        key="kotak"
        kind="disconnected"
        title={kotak.label === 'Session failing' ? 'Kotak session expired' : "Kotak isn't logged in"}
        body={live ? 'New orders and exits are blocked until you reconnect.' : 'The price feed runs on the Kotak session: paper trades stop moving until you reconnect.'}
        action={{ label: connecting ? 'Logging in…' : 'Reconnect Kotak', onClick: () => void reconnect(), disabled: connecting }}
      />,
    );
  }
  const age = status.feed_age_secs;
  if (open && kotak?.level !== 'down' && feed?.level === 'down' && age != null && age > STALE_SECS) {
    banners.push(
      <Banner key="stale" kind="stale" title={`Prices are ${Math.round(age)} s old`} body="Buying from the ladder is paused until fresh ticks arrive." />,
    );
  }
  if (status.expiring.length > 0 && dismissed !== today) {
    banners.push(
      <Banner
        key="expiry"
        kind="expiry"
        title={`${expiringLine(status.expiring)} ${status.expiring.length > 1 ? 'expire' : 'expires'} today`}
        body="Exit in-the-money positions before 15:30 to avoid STT on exercise."
        action={{
          label: 'Dismiss',
          onClick: () => {
            try {
              localStorage.setItem(DISMISSED, today);
            } catch {
              // Private mode: dismissed for this visit.
            }
            setDismissed(today);
          },
        }}
      />,
    );
  }
  return <>{banners}</>;
}
