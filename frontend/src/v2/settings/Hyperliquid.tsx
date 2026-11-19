import { useState } from 'react';
import { checkHlCredentials, getHlLive, putHlCredentials, putHlLiveSettings, removeHlCredentials, type Sent } from '../lib/api';
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
 * Settings → Hyperliquid, the owner's: Paper or LIVE, testnet or mainnet,
 * and an API wallet per network. Either switch changes only while flat;
 * LIVE needs the network's API wallet to have passed its check. LIVE is
 * read-only in this release: the Trade screen shows the Hyperliquid account,
 * and orders stay on paper.
 */
export function Hyperliquid({ serverBase, id, title, large }: { serverBase: string; id?: string; title?: string; large: boolean }) {
  const live = usePoll(() => getHlLive(serverBase), 15_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [confirmLive, setConfirmLive] = useState(false);
  const v = live.data;

  async function run(act: () => Promise<Sent>, fail: string, done?: string): Promise<boolean> {
    setBusy(true);
    const sent = await guard('Changing Hyperliquid needs the admin.', act, fail);
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
  return (
    <Section id={id} title={title} lead="BTC, ETH and gold perps on Hyperliquid. Paper trades on the live market with 10,000 USDC; LIVE is your own Hyperliquid account.">
      <Row title="Mode" caption={v.mode === 'live' ? `LIVE on ${v.network}: the Trade screen shows your Hyperliquid account. Orders are sent in the next release; until then it is read-only.` : 'Paper: orders fill on the live market against your paper wallet.'}>
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
      <Row title="Gold’s balance" caption="Gold trades on trade.xyz’s dex, which keeps its own USDC balance apart from BTC and ETH. Move USDC to it in Hyperliquid’s app; the API wallet can’t move money." />
      <Modal open={confirmLive} onClose={() => setConfirmLive(false)} label="Go LIVE on Hyperliquid" width="lg:max-w-[440px]" locked={busy}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Go LIVE on Hyperliquid {v.network}?</h2>
          <p className="type-body text-fg-muted">
            The Trade screen shows your Hyperliquid account {cred ? `(${cred.account})` : ''} instead of the paper wallet. {v.network === 'mainnet' ? 'This is real money.' : 'Testnet uses free test USDC; the charts stay on mainnet prices.'} Orders are not sent in this release. The API wallet is checked again now, and the account must hold and rest nothing on Hyperliquid, so LIVE can be left again.
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
