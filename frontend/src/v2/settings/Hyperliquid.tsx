import { useState } from 'react';
import { useIdentity } from '../auth/identity';
import { checkHlCredentials, engageHlKill, getHlLive, putHlCredentials, putHlLiveSettings, removeHlCredentials, resetHlKill, type Sent } from '../lib/api';
import type { HlCredentialView } from '../lib/types';
import { usePoll } from '../lib/usePoll';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { Segment } from '../ui/Segment';
import { useToast } from '../ui/toast';
import { Row, Section } from './parts';

type Net = 'testnet' | 'mainnet';

const APP_URL: Record<Net, string> = {
  testnet: 'https://app.hyperliquid-testnet.xyz/API',
  mainnet: 'https://app.hyperliquid.xyz/API',
};

/** One network's API wallet: what is stored and its check, or a form to add one. */
function Wallet({ net, cred, canStore, live, busy, onSave, onCheck, onRemove, large }: {
  net: Net;
  cred: HlCredentialView | null;
  canStore: boolean;
  /** It is the LIVE one: it can't be changed or removed. */
  live: boolean;
  busy: boolean;
  onSave: (account: string, key: string) => Promise<boolean>;
  onCheck: () => void;
  onRemove: () => void;
  large: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [account, setAccount] = useState('');
  const [key, setKey] = useState('');
  const field = `w-full border border-line bg-inset px-3 type-body text-fg outline-none focus:border-accent ${large ? 'h-11 rounded-8' : 'h-9 rounded-6'}`;
  const size = large ? 'lg' : 'md';
  const form = (
    <div className="flex w-full flex-col gap-2">
      <input className={field} placeholder="API wallet private key (0x…), from Hyperliquid's API page" type="password" value={key} onChange={(e) => setKey(e.target.value)} autoComplete="new-password" spellCheck={false} />
      <input className={field} placeholder="Your main wallet's address (optional: read from the API wallet)" value={account} onChange={(e) => setAccount(e.target.value)} autoComplete="off" spellCheck={false} />
      <div className="flex gap-2">
        <Button
          size={size}
          kind="accent"
          disabled={busy || !canStore || key.trim() === ''}
          onClick={async () => {
            if (await onSave(account, key)) {
              // The saved account shows from what was stored; nothing typed
              // here carries over to the next Replace.
              setKey('');
              setAccount('');
              setEditing(false);
            }
          }}
        >
          Save and check
        </Button>
        {cred && (
          <Button size={size} kind="neutral" disabled={busy} onClick={() => setEditing(false)}>
            Cancel
          </Button>
        )}
      </div>
    </div>
  );
  return (
    <Row
      stacked
      title={`API wallet · ${net}`}
      caption={
        <>
          Create one at <a className="text-accent underline" href={APP_URL[net]} target="_blank" rel="noreferrer">{APP_URL[net].replace('https://', '')}</a>, authorize it with your main wallet, and paste its private key here. Your account (the main wallet's address, not the API wallet's) is read from Hyperliquid. An API wallet can trade but never withdraw or transfer; the key is sealed on the server with its AUTH_SECRET and never shown again.
          {!canStore && <span className="text-loss"> AUTH_SECRET isn't set on the server, so no key can be stored.</span>}
        </>
      }
    >
      {cred && !editing ? (
        <div className="flex w-full flex-col gap-2">
          <div className="type-caption tnum text-fg-muted">
            Account <span className="text-fg">{cred.account}</span> · API wallet <span className="text-fg">{cred.agent}</span>
          </div>
          <div className={`type-caption ${cred.ok ? 'text-profit' : 'text-loss'}`}>
            {cred.ok ? 'Checked' : 'Not working'}
            {cred.checked_at ? ` · ${cred.checked_at}` : ''}
            {cred.note ? ` — ${cred.note}` : ''}
          </div>
          <div className="flex gap-2">
            <Button size={size} kind="neutral" disabled={busy} onClick={onCheck}>
              Check again
            </Button>
            <Button size={size} kind="ghost" disabled={busy || live} title={live ? 'Switch to Paper first' : undefined} onClick={() => setEditing(true)}>
              Replace
            </Button>
            <Button size={size} kind="ghost" disabled={busy || live} title={live ? 'Switch to Paper first' : undefined} onClick={onRemove}>
              Remove
            </Button>
          </div>
        </div>
      ) : (
        form
      )}
    </Row>
  );
}

/**
 * Settings → Hyperliquid, each person's own: Paper or LIVE, testnet or
 * mainnet, and an API wallet per network. Either switch changes only while
 * flat; LIVE needs the network's API wallet to have passed its check. LIVE
 * is their own Hyperliquid account: the Trade screen shows it, and its
 * orders go to it (`hyperliquid/live/trade.rs`). A user may set it up only
 * once the admin has switched LIVE on for them, and has a kill switch of
 * their own (the admin's is the app's, in Trading).
 */
export function Hyperliquid({ serverBase, id, title, large }: { serverBase: string; id?: string; title?: string; large: boolean }) {
  const live = usePoll(() => getHlLive(serverBase), 15_000, serverBase);
  const { role } = useIdentity();
  const guard = useGuard();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [confirmLive, setConfirmLive] = useState(false);
  const [confirmKill, setConfirmKill] = useState(false);
  const v = live.data;

  async function run(act: () => Promise<Sent>, fail: string, done?: string): Promise<boolean> {
    setBusy(true);
    // Each person's own desk: the admin's and every user's alike.
    const sent = await guard('Changing Hyperliquid needs you to sign in.', act, fail, 'signed');
    setBusy(false);
    await live.refresh();
    if (sent?.ok && done) toast({ tone: 'neutral', title: done, body: '' });
    return !!sent?.ok;
  }

  if (!v) {
    return (
      <Section id={id} title={title}>
        <Row title="Hyperliquid" caption={live.error ?? 'Loading…'} />
      </Section>
    );
  }
  const segSize = large ? 'lg' : 'sm';
  const cred = v.credentials[v.network];
  const lead = 'Perps on Hyperliquid. Paper trades on the live market with 10,000 USDC; LIVE is your own Hyperliquid account.';
  // A user the admin hasn't let trade LIVE, and who isn't LIVE: paper only.
  if (!v.allowed && v.mode !== 'live') {
    return (
      <Section id={id} title={title} lead={lead}>
        <Row title="LIVE" caption="LIVE isn't switched on for you. The admin turns it on; until then you trade perps on paper, which needs nothing set up here." />
      </Section>
    );
  }
  const ownKill = role !== 'admin';
  return (
    <Section id={id} title={title} lead={lead}>
      {!v.allowed && <Row title="LIVE switched off" caption="The admin has switched LIVE off for you: your Hyperliquid account takes no new order that opens. Close and cancel what is there, then switch to Paper." />}
      <Row title="Mode" caption={v.mode === 'live' ? `LIVE on ${v.network}: the Trade screen shows your Hyperliquid account, and its orders go to it${v.network === 'mainnet' ? ' — real money, each opening order confirmed first' : ''}. Stops and targets rest on Hyperliquid.` : 'Paper: orders fill on the live market against your paper wallet.'}>
        <Segment
          label="Hyperliquid mode"
          size={segSize}
          value={v.mode}
          disabled={busy}
          onChange={(mode) => (mode === 'live' ? setConfirmLive(true) : void run(() => putHlLiveSettings(serverBase, { mode }), 'Still LIVE', 'Hyperliquid on Paper'))}
          options={[
            { value: 'paper', label: 'Paper' },
            { value: 'live', label: 'LIVE', kind: 'live' },
          ]}
        />
      </Row>
      <Row title="Network" caption={v.mode === 'live' ? 'Switch to Paper to change it.' : 'Testnet uses Hyperliquid’s free test USDC; mainnet, your funded wallet.'}>
        <Segment
          label="Hyperliquid network"
          size={segSize}
          value={v.network}
          disabled={busy || v.mode === 'live'}
          onChange={(network) => void run(() => putHlLiveSettings(serverBase, { network }), 'Network not changed', `Hyperliquid on ${network}`)}
          options={[
            { value: 'testnet', label: 'Testnet' },
            { value: 'mainnet', label: 'Mainnet' },
          ]}
        />
      </Row>
      {(['testnet', 'mainnet'] as const).map((net) => (
        <Wallet
          key={net}
          net={net}
          cred={v.credentials[net]}
          canStore={v.can_store_keys}
          live={v.mode === 'live' && v.network === net}
          busy={busy}
          large={large}
          onSave={(account, key) => run(() => putHlCredentials(serverBase, { network: net, account: account.trim(), key: key.trim() }), 'API wallet not saved', `${net} API wallet saved`)}
          onCheck={() => void run(() => checkHlCredentials(serverBase, net), 'Check not run')}
          onRemove={() => void run(() => removeHlCredentials(serverBase, net), 'API wallet not removed', `${net} API wallet removed`)}
        />
      ))}
      <Row title="Builder dexes’ balances" caption="Gold and other builder-dex perps (trade.xyz’s and the like) keep their own USDC balance apart from BTC and ETH. Move USDC to them in Hyperliquid’s app; the API wallet can’t move money." />
      {ownKill && (
        <Row
          title="Kill switch"
          caption={
            v.kill
              ? `On: no new LIVE order until you reset it.${v.kill_note ? ` ${v.kill_note}.` : ''}`
              : 'Cancels every order on your Hyperliquid account and closes every position (while LIVE), and refuses new LIVE orders until you reset it. Your account only.'
          }
        >
          {v.kill ? (
            <Button size={large ? 'lg' : 'md'} kind="neutral" disabled={busy} onClick={() => void run(() => resetHlKill(serverBase), 'Kill switch not reset', 'Kill switch reset')}>
              Reset
            </Button>
          ) : (
            <Button size={large ? 'lg' : 'md'} kind="loss" disabled={busy} onClick={() => setConfirmKill(true)}>
              Kill switch
            </Button>
          )}
        </Row>
      )}
      <Modal open={confirmKill} onClose={() => setConfirmKill(false)} label="Hyperliquid kill switch" width="lg:max-w-[440px]" locked={busy}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Close everything on Hyperliquid?</h2>
          <p className="type-body text-fg-muted">
            {v.mode === 'live'
              ? `Every order on your Hyperliquid ${v.network} account is cancelled and every position closed at the market, and no new LIVE order is taken until you reset the switch.`
              : 'You are on Paper: nothing is closed, but no LIVE order is taken until you reset the switch.'}
          </p>
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="neutral" onClick={() => setConfirmKill(false)} disabled={busy} data-autofocus>
              Cancel
            </Button>
            <Button
              size="md"
              kind="loss"
              disabled={busy}
              onClick={async () => {
                if (await run(() => engageHlKill(serverBase), 'Kill switch not engaged', 'Kill switch on')) setConfirmKill(false);
              }}
            >
              Close everything
            </Button>
          </div>
        </div>
      </Modal>
      <Modal open={confirmLive} onClose={() => setConfirmLive(false)} label="Go LIVE on Hyperliquid" width="lg:max-w-[440px]" locked={busy}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Go LIVE on Hyperliquid {v.network}?</h2>
          <p className="type-body text-fg-muted">
            The Trade screen shows your Hyperliquid account {cred ? `(${cred.account})` : ''} instead of the paper wallet. {v.network === 'mainnet' ? 'This is real money.' : 'Testnet uses free test USDC; the charts stay on mainnet prices.'} Its orders go to Hyperliquid, and {ownKill ? 'your kill switch closes it' : 'the kill switch closes it too'}. The API wallet is checked again now, and the account must hold and rest nothing on Hyperliquid, so LIVE can be left again.
          </p>
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="neutral" onClick={() => setConfirmLive(false)} disabled={busy} data-autofocus>
              Cancel
            </Button>
            <Button
              size="md"
              kind="live"
              disabled={busy}
              onClick={async () => {
                if (await run(() => putHlLiveSettings(serverBase, { mode: 'live' }), 'Still on Paper', `Hyperliquid LIVE on ${v.network}`)) setConfirmLive(false);
              }}
            >
              Go LIVE
            </Button>
          </div>
        </div>
      </Modal>
    </Section>
  );
}
