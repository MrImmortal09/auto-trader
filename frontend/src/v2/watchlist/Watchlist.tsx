import { useEffect, useState } from 'react';
import { readStored, writeStored } from '../../lib/execute';
import { useIdentity } from '../auth/identity';
import type { Sent } from '../lib/api';
import { dayLabel, plural, price, signedPct, signedPrice, toneOf } from '../lib/format';
import { chartOffWhy, chartSymOf, ladderOf, stockChipOf, targetOf, type TradeIntent } from '../lib/openInstrument';
import type { Quote, Watchlist as WatchlistData, WatchlistAnswer } from '../lib/types';
import { useQuotes } from '../lib/useQuotes';
import { useTickFlash } from '../lib/useTickFlash';
import { useWatchlists, watchlistActions } from '../lib/useWatchlists';
import { rowTitle } from '../shell/searchModel';
import { stockBuyBlocked } from '../trade/model';
import { Chip } from '../ui/Chip';
import { useToast } from '../ui/toast';
import { DeleteDialog, NameDialog } from './dialogs';
import { ListMenu, ListTabs, type ListAction } from './ListTabs';
import { Row, RowHead } from './Row';

// The Watchlist screen (`/v2/watchlist`, plan Phase 2): NIFTY, BANKNIFTY and
// SENSEX on a strip at the top, then the person's lists — up to five, as a
// Segment — and the open list's rows, priced from the shared quotes poll
// and flashing as they tick. Off hours the rows show the last close under a
// "Closed" chip. Anyone signed in has their own; the admin's are the
// admin's. Read-only market data: nothing here places an order — Buy opens
// the ladder on the Trade screen.

/** The strip's indices, under the keys their spot ticks on. */
const STRIP: { index: string; key: string }[] = [
  { index: 'NIFTY', key: 'idx|NIFTY 50' },
  { index: 'BANKNIFTY', key: 'idx|NIFTY BANK' },
  { index: 'SENSEX', key: 'idx|SENSEX' },
];

/** The list this browser had open, per browser like the rest of v2's view preferences. */
const STORE_KEY = 'v2.watchlist';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg-muted' } as const;

const nowMs = () => Date.now();

/** Unix ms, refreshed every `everyMs`, to age prices against. */
function useNow(everyMs: number): number {
  const [now, setNow] = useState(nowMs);
  useEffect(() => {
    const t = window.setInterval(() => setNow(nowMs()), everyMs);
    return () => window.clearInterval(t);
  }, [everyMs]);
  return now;
}

/** One index on the strip: its level flashing as it ticks, the day's change;
 * a press opens its chart and ladder on the Trade screen. */
function IndexTile({ index, quote, desktop, onOpen }: { index: string; quote: Quote | null; desktop: boolean; onOpen: () => void }) {
  const flash = useTickFlash(quote?.ltp, true, index);
  const points = quote ? quote.change ?? (quote.close != null ? quote.ltp - quote.close : null) : null;
  const pct = quote ? quote.per_change ?? (quote.close ? ((quote.ltp - quote.close) / quote.close) * 100 : null) : null;
  const tone = TONE_TEXT[toneOf(points)];
  return (
    <button
      type="button"
      onClick={onOpen}
      title={`Open the ${index} chart and ladder`}
      className={`flex min-w-0 flex-col gap-1 rounded-12 border border-line bg-panel text-left transition-colors duration-120 hover:bg-raised/40 ${desktop ? 'px-4 py-3' : 'px-3 py-2'}`}
    >
      <span className="type-eyebrow text-fg-muted">{index}</span>
      <span className={`flex min-w-0 ${desktop ? 'flex-wrap items-baseline gap-x-2 gap-y-0.5' : 'flex-col'}`}>
        <span key={flash.key} className={`-mx-1 rounded-4 px-1 ${desktop ? 'num-large' : 'num-medium'} ${quote ? 'text-fg' : 'text-fg-subtle'} ${flash.className}`}>
          {quote ? price(quote.ltp) : '—'}
        </span>
        {pct != null && (
          <span className={`num-small ${tone}`}>
            {desktop && points != null && <span className="mr-1.5">{signedPrice(points)}</span>}
            {signedPct(pct)}
          </span>
        )}
      </span>
    </button>
  );
}

type Dialog = { kind: 'new' } | { kind: 'rename'; list: WatchlistData } | { kind: 'delete'; list: WatchlistData };

export function Watchlist({ serverBase, desktop, stocksOn = false, stockPaperOn = false, onOpenTrade, onOpenInstrument }: {
  serverBase: string;
  desktop: boolean;
  /** The `stock_charts` switch: a stock's Chart and Chain light up. */
  stocksOn?: boolean;
  /** The `stock_paper` switch: a stock's Buy lights up on a user's paper book. */
  stockPaperOn?: boolean;
  /** Open the Trade screen on a chart and/or the ladder. */
  onOpenTrade: (intent: TradeIntent) => void;
  /** Open an instrument's page by its catalog id. */
  onOpenInstrument: (id: string) => void;
}) {
  const data = useWatchlists(serverBase, { poll: true });
  const toast = useToast();
  // Whose desk a Buy would trade on: the owner's engine for the admin, a
  // paper book for anyone else (the Trade screen asks a visitor to sign in).
  const { canReadOwner } = useIdentity();
  const paper = !canReadOwner;
  const [picked, setPicked] = useState<number | null>(() => readStored<{ selected?: number }>(STORE_KEY, {}).selected ?? null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const lists = data.lists;
  const list = lists.find((l) => l.id === picked) ?? lists[0] ?? null;
  const items = list?.items ?? [];
  // The strip's three and the open list's rows, one poll for all of them.
  const quotes = useQuotes(serverBase, [...STRIP.map((s) => s.key), ...items.map((i) => i.key)]);
  const marketOpen = quotes.marketOpen ?? data.answer?.market_open ?? true;
  // The server's time: a quote's `at` is its clock's, and this browser's may
  // run fast (every row grey) or slow (a stale row looking fresh).
  const now = useNow(5000) + quotes.clockOffset;
  const closedAsOf = !marketOpen ? [...STRIP.map((s) => quotes.get(s.key)?.as_of), ...items.map((i) => quotes.get(i.key)?.as_of ?? i.quote?.as_of)].find((s) => !!s) ?? null : null;

  function select(id: number) {
    setPicked(id);
    writeStored(STORE_KEY, { selected: id });
  }

  /** Send a change; a refusal is a toast titled `failTitle`, its body what
   * `explain` makes of it or else the server's reason. */
  async function act(failTitle: string, run: () => Promise<Sent>, explain?: (sent: Sent) => string | null): Promise<Sent> {
    const sent = await run();
    // A 401 already dropped the sign-in, which brings back the sign-in page (see lib/api).
    if (!sent.ok && sent.status !== 401) toast({ tone: 'loss', title: failTitle, body: explain?.(sent) ?? sent.error ?? 'The server refused it.' });
    return sent;
  }

  function onListAction(action: ListAction) {
    if (!list) return;
    const at = lists.findIndex((l) => l.id === list.id);
    if (action === 'rename') setDialog({ kind: 'rename', list });
    else if (action === 'delete') setDialog({ kind: 'delete', list });
    else void act("Couldn't move the watchlist", () => watchlistActions.move(list.id, action === 'left' ? at - 1 : at + 1));
  }

  function move(key: string, to: 'top' | 'up' | 'down') {
    if (!list) return;
    const keys = items.map((i) => i.key);
    const at = keys.indexOf(key);
    if (at < 0) return;
    const next = at + (to === 'up' ? -1 : to === 'down' ? 1 : -at);
    if (next < 0 || next >= keys.length || next === at) return;
    keys.splice(at, 1);
    keys.splice(next, 0, key);
    // The server keeps the keys a list holds as they are, so an expired
    // contract in it never blocks a reorder; a server from before that
    // refuses the whole list (400, "unknown keys") while it holds one.
    const dead = items.filter((i) => !i.instrument).length;
    void act("Couldn't move it", () => watchlistActions.setItems(list.id, keys), (sent) =>
      dead > 0 && sent.status === 400 ? `This list holds ${dead === 1 ? 'an expired contract' : `${dead} expired contracts`} the server can't reorder around. Remove the expired ${dead === 1 ? 'row' : 'rows'} (No longer listed) first, then move it again.` : null,
    );
  }

  const pad = desktop ? 'px-6 py-5' : 'px-4 py-4';
  const empty = !data.answer && !data.error;

  return (
    <div className={`mx-auto flex w-full max-w-[960px] flex-col gap-5 ${pad}`}>
      <section aria-label="Indices" className="grid grid-cols-3 gap-3">
        {STRIP.map(({ index, key }) => (
          <IndexTile key={index} index={index} quote={quotes.get(key)} desktop={desktop} onOpen={() => onOpenTrade({ chart: { kind: 'index', index }, ladder: { index } })} />
        ))}
      </section>

      {empty ? (
        <p className="type-body text-fg-subtle">Loading…</p>
      ) : !data.answer ? (
        <p className="type-body text-loss">Couldn't load the watchlists: {data.error}</p>
      ) : (
        list && (
          <section aria-label="Watchlist" className="flex flex-col gap-3">
            {(() => {
              const tabs = <ListTabs lists={lists} selected={list.id} desktop={desktop} onSelect={select} />;
              const menu = <ListMenu lists={lists} selected={list.id} maxLists={data.answer.max_lists} desktop={desktop} onNew={() => setDialog({ kind: 'new' })} onAction={onListAction} />;
              const note = (
                <div className="flex min-w-0 flex-wrap items-center gap-2 type-caption text-fg-subtle tnum">
                  <span>
                    {plural(items.length, 'instrument')} of {data.answer.max_items}
                  </span>
                  {!marketOpen && (
                    <>
                      <Chip kind="neutral">Closed</Chip>
                      <span>{closedAsOf ? `prices as of ${closedAsOf.slice(11, 16)}, ${dayLabel(closedAsOf)}` : 'the last close'}</span>
                    </>
                  )}
                  {data.error && <span className="text-loss">· {data.error}</span>}
                </div>
              );
              return desktop ? (
                <div className="flex flex-wrap items-center justify-between gap-3">
                  <div className="flex min-w-0 items-center gap-2">
                    {tabs}
                    {menu}
                  </div>
                  {note}
                </div>
              ) : (
                <div className="flex flex-col gap-2">
                  {tabs}
                  <div className="flex items-center justify-between gap-3">
                    {note}
                    {menu}
                  </div>
                </div>
              );
            })()}

            {/* The rows read off white: a card on desktop, edge to edge on a phone. */}
            <div className={`bg-panel ${desktop ? 'rounded-12 border border-line px-4' : '-mx-4 border-y border-line'}`}>
              {desktop && items.length > 0 && <RowHead />}
              {items.length === 0 ? (
                <p className={`type-body text-fg-subtle ${desktop ? 'py-6' : 'px-4 py-6'}`}>Nothing here yet. Add instruments with the + on a search result or an instrument page.</p>
              ) : (
                items.map((item, n) => {
                  const row = item.instrument;
                  const ladder = row ? ladderOf(row, stocksOn) : null;
                  const chart = row ? chartSymOf(row, stocksOn) : null;
                  const chip = row ? stockChipOf(row) : undefined;
                  // Buy: an index's ladder for anyone; a stock's for a user's paper
                  // book behind `stock_paper`, never from the owner's desk.
                  const buyWhy = ladder && chip ? stockBuyBlocked('stock', paper, stockPaperOn) ?? undefined : undefined;
                  const buyLadder = ladder && !buyWhy ? ladder : null;
                  const title = row ? rowTitle(row) : item.key;
                  return (
                    <Row
                      key={item.key}
                      item={item}
                      quote={quotes.get(item.key) ?? item.quote}
                      desktop={desktop}
                      first={n === 0}
                      last={n === items.length - 1}
                      marketOpen={marketOpen}
                      now={now}
                      actions={{
                        onOpen: () => {
                          if (!row) return;
                          const target = targetOf(row, 'page', stocksOn);
                          if ('trade' in target) onOpenTrade(target.trade);
                          else onOpenInstrument(target.instrument);
                        },
                        onChart: chart ? () => onOpenTrade({ chart, ladder: ladder ?? undefined, stock: chip }) : null,
                        chartWhy: row && !chart ? chartOffWhy(row, stocksOn) : undefined,
                        onChain: ladder ? () => onOpenTrade({ ladder, stock: chip }) : null,
                        chainWhy: stocksOn && row && !row.has_fo ? 'No options listed for this stock' : undefined,
                        onBuy: buyLadder ? () => onOpenTrade({ ladder: buyLadder, stock: chip }) : null,
                        buyWhy,
                        onMove: (to) => move(item.key, to),
                        onRemove: () => void act(`Couldn't remove ${title}`, () => watchlistActions.removeItem(list.id, item.key)),
                      }}
                    />
                  );
                })
              )}
            </div>
          </section>
        )
      )}

      {dialog?.kind === 'new' && data.answer && (
        <NameDialog
          desktop={desktop}
          title="New watchlist"
          maxChars={data.answer.max_name_chars}
          submitLabel="Create"
          onSubmit={async (name) => {
            const sent = await act("Couldn't create the watchlist", () => watchlistActions.create(name));
            if (sent.ok) {
              const made = (sent.body as WatchlistAnswer | null)?.watchlist;
              if (made) select(made.id);
            }
            return sent.ok;
          }}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === 'rename' && data.answer && (
        <NameDialog
          desktop={desktop}
          title={`Rename '${dialog.list.name}'`}
          current={dialog.list.name}
          maxChars={data.answer.max_name_chars}
          submitLabel="Rename"
          onSubmit={async (name) => (await act("Couldn't rename the watchlist", () => watchlistActions.rename(dialog.list.id, name))).ok}
          onClose={() => setDialog(null)}
        />
      )}
      {dialog?.kind === 'delete' && (
        <DeleteDialog
          desktop={desktop}
          list={dialog.list}
          onConfirm={async () => (await act("Couldn't delete the watchlist", () => watchlistActions.remove(dialog.list.id))).ok}
          onClose={() => setDialog(null)}
        />
      )}
    </div>
  );
}
