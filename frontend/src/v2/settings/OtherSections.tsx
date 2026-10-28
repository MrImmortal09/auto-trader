import { getWalletBalance, putFlags, setWalletBalance } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import type { Flags, SystemAnswer } from '../lib/types';
import { useGuard } from '../shell/useGuard';
import { NumberInput } from '../ui/NumberInput';
import { Toggle } from '../ui/Toggle';
import { useToast } from '../ui/toast';
import { Row, Section } from './parts';
import { uptimeText } from './summary';
import { UpdateServer } from './UpdateServer';
import type { SettingsState } from './useSettings';

/** Trade: how its orders go, which is fixed. */
export function TradeNotes({ flags, id, title }: { flags: Flags | null; id?: string; title?: string }) {
  return (
    <Section id={id} title={title}>
      <Row title="One-tap buy" caption="A tap on the ladder buys at market, with no confirm. Any strike of any listed expiry; lots per tap are set on the Trade screen." />
      <Row title="Hold to exit all" caption="Holding Exit all for a second sells every manual trade at market. Telegram positions are left alone." />
      <Row title="Pointer only" caption="No key on the keyboard ever sends an order." />
      <Row title="Chart stops and targets" caption={flags?.chart_stops ? 'On: drag a scalp’s stop and target on its charts.' : 'Off: turn it on in Beta to drag levels on the charts.'} />
    </Section>
  );
}

/** Server and data: what's running, the paper wallet and the P&L charges. */
export function ServerData({ serverBase, settings, system, onRestarted, id, title, large = false }: {
  serverBase: string;
  settings: SettingsState;
  system: SystemAnswer | null;
  onRestarted: () => void;
  id?: string;
  title?: string;
  large?: boolean;
}) {
  const { cfg, save } = settings;
  const wallet = usePoll(() => getWalletBalance(serverBase), 30_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  if (!cfg) return null;
  const size = large ? 'lg' : 'md';
  const paper = cfg.mode !== 'LIVE';
  const running = system
    ? [`v${system.version}`, system.uptime_secs != null ? `up ${uptimeText(system.uptime_secs)}` : null, system.scrip_loaded_at ? `Scrip Master loaded ${system.scrip_loaded_at.slice(11, 16)}` : 'Scrip Master not loaded']
        .filter(Boolean)
        .join(' · ')
    : '…';

  async function setBalance(n: number) {
    const sent = await guard('Changing the paper wallet needs write access.', () => setWalletBalance(serverBase, n), "Couldn't set the paper wallet");
    if (sent?.ok) toast({ tone: 'neutral', title: 'Saved', body: `Paper wallet ₹${n.toLocaleString('en-IN')}` });
    await wallet.refresh();
  }

  return (
    <Section id={id} title={title}>
      <Row title="Server" caption={running} />
      <Row title="Paper wallet" caption={paper ? 'Paper buys, sells and charges settle against it.' : 'Not used in LIVE.'}>
        {wallet.data && (
          <NumberInput label="Paper wallet" value={wallet.data.balance} min={0} max={1_000_000_000} grouped unit="₹" size={size} width="w-[150px]" disabled={!paper} onCommit={(n) => void setBalance(n)} />
        )}
      </Row>
      <Row title="Brokerage per order" caption="A flat charge per order in the P&L figures. What Kotak charges doesn't change.">
        <NumberInput label="Brokerage per order" value={cfg.brokerage_per_order} min={0} max={1000} grouped unit="₹" size={size} onCommit={(n) => void save(() => ({ brokerage_per_order: n }), `Brokerage ₹${n} an order`)} />
      </Row>
      <Row title="Update the server" caption="The latest release from GitHub. It restarts in about a minute, and goes back to this one if the new one fails.">
        <UpdateServer serverBase={serverBase} size={size} onRestarted={onRestarted} />
      </Row>
      <Row title="Clear the database" caption="In the old design, which asks first.">
        <a href="/v1/settings" className="inline-flex h-7 items-center rounded-6 bg-raised px-2.5 type-button text-fg hover:bg-line">
          Open
        </a>
      </Row>
    </Section>
  );
}

/** Beta: the two engine switches, off until turned on (`/api/v2/flags`). */
export function Beta({ serverBase, flags, onSaved, id, title }: { serverBase: string; flags: Flags | null; onSaved: () => void; id?: string; title?: string }) {
  const guard = useGuard();
  const toast = useToast();
  async function flip(change: Partial<Flags>, what: string) {
    const sent = await guard('Changing a Beta switch needs write access.', () => putFlags(serverBase, change), "Couldn't change the switch");
    if (sent?.ok) toast({ tone: 'neutral', title: 'Saved', body: what });
    onSaved();
  }
  return (
    <Section id={id} title={title}>
      <Row title="Index options only" caption="Skip calls for anything but index options; they show as skipped in the Telegram feed.">
        <Toggle label="Index options only" checked={flags?.index_options_only ?? false} disabled={!flags} onChange={(on) => void flip({ index_options_only: on }, `Index options only ${on ? 'on' : 'off'}`)} />
      </Row>
      <Row title="Chart stops" caption="Set a manual trade's stop and target by dragging on the Trade charts. A level crossed sells the whole holding at market.">
        <Toggle label="Chart stops" checked={flags?.chart_stops ?? false} disabled={!flags} onChange={(on) => void flip({ chart_stops: on }, `Chart stops ${on ? 'on' : 'off'}`)} />
      </Row>
    </Section>
  );
}
