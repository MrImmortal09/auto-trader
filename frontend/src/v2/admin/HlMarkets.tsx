import { useState } from 'react';
import { ChevronDown, ChevronRight, Pin } from 'lucide-react';
import { getFlags, getHlAdminMarkets, getHlCatalog, putHlAdminMarkets, type HlMarketsChange } from '../lib/api';
import { signedPct, toneOf } from '../lib/format';
import type { HlAdminDex, HlAdminMarkets, HlAdminPerp } from '../lib/types';
import { usePoll } from '../lib/usePoll';
import { Row, Section } from '../settings/parts';
import { useGuard } from '../shell/useGuard';
import { setPerpCatalog } from '../trade/perp/catalog';
import { Button } from '../ui/Button';
import { Chip } from '../ui/Chip';
import { Modal } from '../ui/Modal';
import { Segment } from '../ui/Segment';
import { useToast } from '../ui/toast';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;
const INPUT = 'rounded-6 border border-line bg-inset px-2.5 text-fg transition-colors duration-120 placeholder:text-fg-subtle focus:border-accent';

/** "$609M", "$36.2M", "$410K". */
function compactUsd(n: number | null): string {
  if (n == null) return '—';
  if (n >= 1e9) return `$${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `$${(n / 1e6).toFixed(n >= 1e8 ? 0 : 1)}M`;
  if (n >= 1e3) return `$${(n / 1e3).toFixed(0)}K`;
  return `$${n.toFixed(0)}`;
}

/** A price with as many decimals as it needs. */
function px(n: number | null): string {
  if (n == null) return '—';
  return n.toLocaleString('en-US', { maximumFractionDigits: n >= 100 ? 2 : n >= 1 ? 4 : 6 });
}

/** "0.009 %", the taker fee as the admin reads it. */
const pct = (f: number) => `${(f * 100).toFixed(f * 100 >= 0.01 ? 3 : 4)}%`;

/** What a removal does to people holding it. */
const REMOVE_WORDS =
  'Removed perps leave the charts and search. Anyone holding one keeps the position, with its stop, targets and liquidation, until they close it, but it takes no new orders that open or add, and resting opening orders and line orders in it are cancelled.';

/**
 * Admin → Hyperliquid markets (behind the `hyperliquid` switch): every perp
 * Hyperliquid lists, grouped by venue, and the ones the app trades. An
 * added perp is charted, found by search and traded on paper by everyone;
 * the pin puts it in the Trade header.
 */
export function HlMarkets({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const flags = usePoll(() => getFlags(serverBase), 30_000, serverBase);
  const on = flags.data?.hyperliquid === true;
  const view = usePoll(() => getHlAdminMarkets(serverBase), 60_000, serverBase, on);
  const [fresh, setFresh] = useState<HlAdminMarkets | null>(null);
  const [filter, setFilter] = useState('');
  const [show, setShow] = useState<'all' | 'added'>('all');
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<{ change: HlMarketsChange; what: string } | null>(null);
  const guard = useGuard();
  const toast = useToast();
  const size = desktop ? 'sm' : 'lg';

  if (!on) return null;
  // The answer to the last change is newer than the poll's until the next poll.
  const v = fresh && (!view.data || fresh.rev >= view.data.rev) ? fresh : view.data;

  async function change(c: HlMarketsChange, done: string) {
    setBusy(true);
    const sent = await guard('Changing the markets needs the admin.', () => putHlAdminMarkets(serverBase, c), 'Markets not changed');
    setBusy(false);
    setConfirm(null);
    if (!sent?.ok) return;
    if (sent.body) setFresh(sent.body as HlAdminMarkets);
    toast({ tone: 'neutral', title: done, body: '' });
    // This page's own charts and search read the new list at once.
    getHlCatalog(serverBase)
      .then((a) => setPerpCatalog(a.rev, a.markets))
      .catch(() => {});
  }

  /** A removal asks first when someone holds what goes. */
  function remove(perps: HlAdminPerp[], what: string) {
    const coins = perps.filter((p) => p.state === 'added').map((p) => p.coin);
    if (coins.length === 0) return;
    const c = { remove: coins };
    if (perps.some((p) => p.held) || coins.length > 1) setConfirm({ change: c, what });
    else void change(c, `${what} removed`);
  }

  if (!v) {
    return (
      <Section title="Hyperliquid markets">
        <Row title={view.error ? `Couldn't load Hyperliquid's markets: ${view.error}` : 'Loading Hyperliquid’s markets…'} />
      </Section>
    );
  }

  const q = filter.trim().toLowerCase();
  const matches = (p: HlAdminPerp) => (show === 'all' || p.state !== 'off') && (!q || p.base.toLowerCase().includes(q) || p.coin.toLowerCase().includes(q));
  const all = v.dexes.flatMap((d) => d.perps);
  const added = all.filter((p) => p.state === 'added').length;
  const pinned = all.filter((p) => p.header).length;

  const perpRow = (p: HlAdminPerp) => {
    const change_ = p.change_pct;
    return (
      <div key={p.coin} className="flex min-h-11 items-center gap-3 border-t border-line py-1.5 pr-3 pl-4 first:border-t-0">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="type-body-strong text-fg">{p.base}</span>
            {p.coin !== p.base && <span className="type-caption text-fg-subtle">{p.coin}</span>}
            {p.state === 'added' && <Chip kind="accent">Added</Chip>}
            {p.state === 'close_only' && <Chip kind="neutral">Close-only · held</Chip>}
            {p.held && p.state === 'added' && <Chip kind="outline">Held</Chip>}
          </div>
          <div className="type-caption tnum text-fg-subtle">
            {px(p.price)}
            {change_ != null && <span className={`ml-1 ${TONE_TEXT[toneOf(change_)]}`}>{signedPct(change_)}</span>} · {compactUsd(p.day_volume)} a day · up to {p.leverage_cap}x
 · taker {pct(p.taker_fee)}
            {p.growth_mode ? ' (growth mode)' : ''}
          </div>
        </div>
        {p.state === 'added' && (
          <button
            type="button"
            disabled={busy || (!p.header && pinned >= v.max_header)}
            onClick={() => void change({ header: [{ coin: p.coin, on: !p.header }] }, p.header ? `${p.base} off the header` : `${p.base} in the header`)}
            title={p.header ? 'In the Trade header: click to take it off' : pinned >= v.max_header ? `At most ${v.max_header} in the header` : 'Put it in the Trade header'}
            aria-label={p.header ? `Take ${p.base} off the header` : `Put ${p.base} in the header`}
            className={`grid size-8 shrink-0 place-items-center rounded-6 hover:bg-raised disabled:opacity-40 ${p.header ? 'text-accent' : 'text-fg-subtle'}`}
          >
            <Pin size={15} fill={p.header ? 'currentColor' : 'none'} />
          </button>
        )}
        {p.state === 'added' ? (
          <Button size={size} kind="quiet" disabled={busy} onClick={() => remove([p], p.base)}>
            Remove
          </Button>
        ) : (
          <Button size={size} kind="neutral" disabled={busy} onClick={() => void change({ add: [p.coin] }, `${p.base} added`)}>
            Add
          </Button>
        )}
      </div>
    );
  };

  const dexBlock = (d: HlAdminDex) => {
    const id = d.dex ?? 'core';
    const rows = d.perps.filter(matches);
    if (q && rows.length === 0) return null;
    const off = d.perps.filter((p) => p.state !== 'added');
    const on_ = d.perps.filter((p) => p.state === 'added');
    // Searching opens every group with a match; otherwise a group opens when clicked.
    const expanded = q ? true : (open[id] ?? false);
    return (
      <div key={id} className="border-t border-line first:border-t-0">
        <div className="flex items-center gap-2 py-2.5 pr-3 pl-2">
          <button type="button" onClick={() => setOpen((o) => ({ ...o, [id]: !expanded }))} aria-expanded={expanded} className="flex min-w-0 flex-1 items-center gap-1.5 text-left">
            {expanded ? <ChevronDown size={16} className="shrink-0 text-fg-muted" /> : <ChevronRight size={16} className="shrink-0 text-fg-muted" />}
            <span className="type-body-strong text-fg">{d.venue}</span>
            <span className="type-caption text-fg-subtle">
              {d.dex ? `${d.dex} · ` : 'crypto · '}
              {on_.length} of {d.perps.length} added
            </span>
          </button>
          {off.length > 0 && (
            <Button size={size} kind="neutral" disabled={busy} onClick={() => void change({ add: off.map((p) => p.coin) }, `${off.length} ${d.venue} perps added`)}>
              Add all
            </Button>
          )}
          {on_.length > 0 && (
            <Button size={size} kind="quiet" disabled={busy} onClick={() => remove(on_, `${on_.length} ${d.venue} perps`)}>
              Remove all
            </Button>
          )}
        </div>
        {expanded && (
          <div className="border-t border-line bg-inset/40">
            {rows.map(perpRow)}
            {rows.length === 0 && <p className="px-4 py-3 type-caption text-fg-subtle">None added yet.</p>}
          </div>
        )}
      </div>
    );
  };

  return (
    <Section
      title="Hyperliquid markets"
      lead={
        <>
          The perps the charts, search and the perps ticket offer, to you and every user’s paper desk. An added perp trades like BTC: long or short, isolated margin up to its own Hyperliquid limit (40x on BTC). The pin puts it in the Trade header (at most {v.max_header}). In LIVE, each builder venue (trade.xyz, Paragon, …) keeps its own USDC balance, funded in Hyperliquid’s app. {REMOVE_WORDS}
        </>
      }
    >
      <div className="flex flex-wrap items-center gap-2 px-4 py-3">
        <input className={`${INPUT} ${desktop ? 'h-8' : 'h-11'} min-w-0 flex-1`} placeholder="Filter: NVDA, SOL, xyz:…" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter the perps" spellCheck={false} autoComplete="off" />
        <Segment
          label="Show"
          size={desktop ? 'sm' : 'lg'}
          value={show}
          onChange={setShow}
          options={[
            { value: 'all', label: 'All' },
            { value: 'added', label: 'Added' },
          ]}
        />
        <span className="w-full type-caption text-fg-subtle tnum">
          {added} of {all.length} added · {pinned} in the header · Hyperliquid’s list as of the last 5 minutes
        </span>
      </div>
      <div className="border-t border-line">{v.dexes.map(dexBlock)}</div>
      {v.unlisted.length > 0 && (
        <Row
          title="No longer listed by Hyperliquid"
          caption={v.unlisted.map((u) => `${u.label}${u.held ? ' (held)' : ''}`).join(', ')}
        />
      )}
      <Modal open={confirm != null} onClose={() => setConfirm(null)} label="Remove perps" width="lg:max-w-[440px]" locked={busy}>
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">Remove {confirm?.what}?</h2>
          <p className="type-body text-fg-muted">{REMOVE_WORDS}</p>
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="neutral" onClick={() => setConfirm(null)} disabled={busy} data-autofocus>
              Cancel
            </Button>
            <Button size="md" kind="loss" disabled={busy} onClick={() => confirm && void change(confirm.change, `${confirm.what} removed`)}>
              Remove
            </Button>
          </div>
        </div>
      </Modal>
    </Section>
  );
}
