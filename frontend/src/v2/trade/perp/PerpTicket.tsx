import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Check, Minus, Plus, X } from 'lucide-react';
import type { HlMarket, PerpAccount, PerpSide } from '../../lib/types';
import { perpInfo } from './catalog';
import { signedPct, toneOf } from '../../lib/format';
import { Button } from '../../ui/Button';
import { Modal } from '../../ui/Modal';
import { Segment } from '../../ui/Segment';
import { Toggle } from '../../ui/Toggle';
import { useToast } from '../../ui/toast';
import { C } from '../chartSetup';
import type { PerpKey } from '../model';
import { setPerpDraft } from './draft';
import { estLiq, leverageCap, perpLabel, perpPrice, pnlAt, roundPrice, roundSize, rulesOf, signedUsd, sizeText, usd, VALIDITIES } from './model';
import type { Perps } from './usePerps';

/** What the size field is typed in: dollars of margin (the capital put up;
 * the position is that times the leverage), or the coin. */
type SizeIn = 'usd' | 'coin';
const SIZE_IN_KEY = 'v2.perpSizeIn';

function savedSizeIn(): SizeIn {
  try {
    return localStorage.getItem(SIZE_IN_KEY) === 'coin' ? 'coin' : 'usd';
  } catch {
    return 'usd';
  }
}

/** Whether a stop or target at `at` is on the right side of a `side`
 * position entered at `entry`. */
function onSide(side: PerpSide, what: 'stop' | 'target', entry: number, at: number): boolean {
  if (at === entry) return false;
  const below = at < entry;
  return (what === 'stop') === (side === 'long') ? below : !below;
}

type OrderKind = 'market' | 'limit' | 'line';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** A number typed into a field, or null when it isn't one. */
function typed(text: string): number | null {
  const n = Number(text.replace(/[,$\s]/g, ''));
  return text.trim() !== '' && Number.isFinite(n) && n > 0 ? n : null;
}

/** An inset field with its unit. */
function Field({ label, value, onChange, unit, placeholder, large, action }: { label: string; value: string; onChange: (v: string) => void; unit?: ReactNode; placeholder?: string; large: boolean; action?: ReactNode }) {
  const bad = value.trim() !== '' && typed(value) == null;
  return (
    <label className="flex items-center justify-between gap-3">
      <span className="type-label text-fg-muted">{label}</span>
      <span className="flex items-center gap-1.5">
        {action}
        <span className={`flex w-[150px] items-center gap-2 border bg-inset px-3 focus-within:border-accent ${large ? 'h-11 rounded-8' : 'h-8 rounded-6'} ${bad ? 'border-loss' : 'border-line'}`}>
          <input
            type="text"
            inputMode="decimal"
            autoComplete="off"
            aria-label={label}
            aria-invalid={bad}
            placeholder={placeholder}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className="min-w-0 flex-1 bg-transparent num-medium text-fg outline-none placeholder:text-fg-subtle"
          />
          {unit && <span className="shrink-0 type-caption text-fg-subtle">{unit}</span>}
        </span>
      </span>
    </label>
  );
}

/**
 * LIVE: the USDC a new position in `k` can use now (Hyperliquid's own
 * figure, for the side picked), and where the account's USDC sits — spot
 * USDC is every perp's margin in a unified account, and must be moved to
 * the perp's balance in a standard one.
 */
function LiveFunds({ account, k, side }: { account: PerpAccount; k: PerpKey; side: PerpSide }) {
  const avail = account.available?.market === k ? (side === 'long' ? account.available.buy : account.available.sell) : null;
  const spot = account.spot_usdc_free ?? 0;
  const unified = account.account_mode === 'unifiedAccount' || account.account_mode === 'portfolioMargin';
  const venue = perpInfo(k)?.venue ?? 'its';
  return (
    <div className="flex flex-col gap-0.5 type-caption tnum">
      <div className="flex justify-between gap-3">
        <span className="text-fg-muted">Available to {side === 'long' ? 'buy' : 'sell'} {perpLabel(k)}</span>
        <span className={avail != null && avail > 0 ? 'text-fg' : 'text-loss'}>{avail == null ? '—' : usd(avail)}</span>
      </div>
      {account.spot_usdc != null && (
        <div className="flex justify-between gap-3">
          <span className="text-fg-muted">Spot USDC</span>
          <span className="text-fg">{usd(account.spot_usdc)}</span>
        </div>
      )}
      {unified ? (
        <p className="text-fg-subtle">Unified account: your spot USDC is the margin for every perp.</p>
      ) : spot > 0 && avail != null && avail < 1 ? (
        <p className="text-loss">Your {usd(spot)} USDC is in Spot. In Hyperliquid's app, move it to Perps{venue !== 'Hyperliquid' ? ` and then to ${venue}'s balance` : ''} to trade {perpLabel(k)}.</p>
      ) : null}
    </div>
  );
}

/**
 * The perp order ticket, in the options ladder's place while a perp is
 * picked: long or short, market or limit, or "Buy on a line" (tap the
 * chart for the line, then its stop and target, both optional); a size in
 * dollars of margin or in the coin, leverage up to the cap, and an optional
 * stop and target the position takes. Paper only for now: it fills on the
 * live Hyperliquid market. Under it, the account: equity, free USDC, margin
 * and open P&L.
 */
export function PerpTicket({ k, market, account, perps, large, onClose }: {
  k: PerpKey;
  market: HlMarket | undefined;
  account: PerpAccount | null;
  perps: Perps;
  /** Phone: 44 pt controls. */
  large: boolean;
  /** Back to the options ladder. */
  onClose?: () => void;
}) {
  const [side, setSide] = useState<PerpSide>('long');
  const [kind, setKind] = useState<OrderKind>('market');
  const [sizeText_, setSizeText] = useState('');
  const [priceText, setPriceText] = useState('');
  const [stopText, setStopText] = useState('');
  const [targetText, setTargetText] = useState('');
  const [leverage, setLeverage] = useState(5);
  const [validity, setValidity] = useState<number | null>(60);
  const [reduceOnly, setReduceOnly] = useState(false);
  const [sending, setSending] = useState(false);
  const [sizeIn, setSizeIn] = useState<SizeIn>(savedSizeIn);
  const toast = useToast();

  const rules = rulesOf(k);
  const cap = leverageCap(k);
  const lev = Math.max(1, Math.min(cap, leverage));
  const mid = market?.mid ?? market?.mark ?? null;
  const held = account?.positions.find((p) => p.market === k) ?? null;
  // An add puts margin up at the position's own leverage; an order the other
  // way first closes what is held, and opens what is left over at the
  // stepper's — the leverage shown is the one sent.
  const adding = held != null && held.side === side;
  const usedLev = adding ? held.leverage : lev;
  const amount = typed(sizeText_);
  const price = kind === 'market' ? mid : typed(priceText) == null ? null : roundPrice(k, typed(priceText)!);
  /** The coin size `amount` comes to for an order at `at`: dollars of margin
   * times the leverage, over the price; or the coin as typed. */
  const sizeAt = (at: number | null, lev_: number): number | null => {
    if (amount == null) return null;
    if (sizeIn === 'coin') return roundSize(k, amount);
    return at != null && at > 0 ? roundSize(k, (amount * lev_) / at) : null;
  };
  const size = sizeAt(price, usedLev);
  const notional = size != null && price != null ? size * price : null;
  // The part of it that opens or adds to a position (an order the other way closes the rest).
  const opening = size == null ? null : held && !adding ? Math.max(0, size - held.size) : size;
  const margin = opening != null && price != null ? (opening * price) / usedLev : null;
  const fee = notional != null ? notional * (kind === 'limit' ? rules.maker : rules.taker) : null;
  // A liquidation price for a position the order opens fresh.
  const liq = price != null && (!held || (!adding && (opening ?? 0) > 0)) ? estLiq(k, side, price, usedLev) : null;
  const stop = typed(stopText);
  const target = typed(targetText);
  const reduce = kind !== 'line' && reduceOnly;
  // LIVE: orders go to the owner's Hyperliquid account. On mainnet each one
  // that opens asks first (the owner's pick); testnet is one click.
  const live = account?.mode === 'live';
  const mainnet = live && account?.network === 'mainnet';
  const [confirm, setConfirm] = useState<{ title: string; lines: string[]; go: () => void } | null>(null);
  // A close-only perp (removed from the list, still held) takes only an
  // order that reduces the position: the other way, no more than is held.
  const closeOnly = rules.known && !rules.tradable;
  const reduces = held != null && held.side !== side && size != null && size <= held.size + 1e-12 && kind !== 'line';
  const ready = rules.known && (!closeOnly || reduces) && size != null && size > 0 && price != null && !sending;
  const size_ = large ? 'lg' : 'md';
  const segSize = large ? 'lg' : 'sm';

  // On the chart while it is set up: where the position this order opens
  // would be liquidated (dotted) once there is something to place — a size
  // for a market order, the price for a limit or a line (right after it is
  // placed) — and only for an order that opens one (not reduce-only, not an
  // add: a held position shows its own); and a line order's line, stop and
  // target until it is armed.
  // LIVE: a resting order that adds to the position can't carry its own
  // stop and target (Hyperliquid would stack a second, fixed-size stop).
  const restingAdd = live && adding && kind !== 'market';
  const draftLiq = !reduce && liq != null && price != null && (kind === 'market' ? size != null && size > 0 : true) ? liq : null;
  const draftMargin = margin != null && margin > 0 ? margin : null;
  const isLine = kind === 'line';
  const draftStop = isLine ? stop : null;
  const draftTarget = isLine ? target : null;
  useEffect(() => {
    const show = price != null && (draftLiq != null || isLine);
    setPerpDraft(show ? { key: k, side, entry: price, leverage: usedLev, liq: draftLiq, margin: draftMargin, line: isLine, size, stop: draftStop, target: draftTarget } : null);
  }, [k, side, price, usedLev, draftLiq, draftMargin, isLine, size, draftStop, draftTarget]);
  useEffect(() => () => setPerpDraft(null), []);

  function chooseSizeIn(next: SizeIn) {
    if (next === sizeIn) return;
    // What is typed keeps its worth: margin at this price and leverage.
    const at = price ?? mid;
    if (amount != null && at != null && at > 0) {
      setSizeText(next === 'coin' ? String(roundSize(k, (amount * usedLev) / at)) : ((amount * at) / usedLev).toFixed(2));
    }
    setSizeIn(next);
    try {
      localStorage.setItem(SIZE_IN_KEY, next);
    } catch {
      // A per-viewer convenience: kept for this visit only.
    }
  }

  // ── Buy on a line: the line, then its stop, then its target, each a tap ── //

  // What the taps read when they land: the ticket as it stands then — the
  // line and stop as its fields show them, corrected by hand between taps
  // or not, so what arms is what the ticket and the chart show.
  const fieldLine = typed(priceText) == null ? null : roundPrice(k, typed(priceText)!);
  const latest = useRef({ side, usedLev, validity, amount, sizeIn, line: fieldLine, stop, live, mainnet });
  useEffect(() => {
    latest.current = { side, usedLev, validity, amount, sizeIn, line: fieldLine, stop, live, mainnet };
  });
  const step = perps.placing?.kind === 'pick' && perps.placing.key === k ? (perps.placing.options?.step ?? null) : null;

  // A pick this ticket started ends with it: closed, or switched to another
  // perp, it leaves no chart taking taps into a ticket that isn't there.
  const placingNow = useRef(perps.placing);
  useEffect(() => {
    placingNow.current = perps.placing;
  });
  const cancelPlacing = perps.cancelPlacing;
  useEffect(
    () => () => {
      if (placingNow.current?.kind === 'pick') cancelPlacing();
    },
    [k, cancelPlacing],
  );

  /** The line as the ticket's field has it now, else where it was tapped. */
  const lineNow = (tapped: number) => latest.current.line ?? tapped;

  /** The coin size the amount comes to on a line at `line`, as the ticket stands now. */
  const coinsOn = (line: number): number | null => {
    const t = latest.current;
    if (t.amount == null) return null;
    return t.sizeIn === 'coin' ? roundSize(k, t.amount) : roundSize(k, (t.amount * t.usedLev) / line);
  };

  /** Arm the line order at `line` with `stopAt` and `targetAt` (either may
   * be null) — on mainnet once the owner confirms it. */
  function armLine(line: number, stopAt: number | null, targetAt: number | null) {
    const t = latest.current;
    const coins = coinsOn(line);
    if (coins == null || coins <= 0) {
      const step_ = `${(10 ** -rules.szDecimals).toFixed(rules.szDecimals)} ${rules.unit}`;
      toast({ tone: 'loss', title: 'Line order not armed', body: t.amount == null ? 'Type the amount first.' : `The amount is too small: at this line it comes to less than one size step (${step_}).` });
      return;
    }
    const go = () => void sendLine(line, stopAt, targetAt, coins);
    if (!t.mainnet) return go();
    const liq = estLiq(k, t.side, line, t.usedLev);
    setConfirm({
      title: `Arm a real ${t.side === 'long' ? 'buy' : 'sell'} on a line?`,
      lines: [
        `${sizeText(k, coins)} ${perpLabel(k)} at ${t.usedLev}x isolated, a market order once the mark ${line > (mid ?? line) ? 'rises' : 'falls'} to ${perpPrice(line)}`,
        `Stop ${stopAt != null ? perpPrice(stopAt) : 'none'} · target ${targetAt != null ? perpPrice(targetAt) : 'none'}`,
        `Margin ≈ ${usd((coins * line) / t.usedLev)}${liq != null ? ` · liquidation ≈ ${perpPrice(roundPrice(k, liq))}` : ''}`,
        'It rests on Hyperliquid until you cancel it.',
      ],
      go,
    });
  }

  async function sendLine(line: number, stopAt: number | null, targetAt: number | null, coins: number) {
    const t = latest.current;
    setSending(true);
    const ok = await perps.actions.line({
      market: k,
      side: t.side === 'long' ? 'buy' : 'sell',
      size: coins,
      leverage: t.usedLev,
      price: line,
      // A LIVE line order rests on Hyperliquid until cancelled.
      valid_minutes: t.live ? null : t.validity,
      stop: stopAt ?? undefined,
      targets: targetAt != null ? [{ price: targetAt, size: null }] : undefined,
    });
    setSending(false);
    if (ok) {
      setPriceText('');
      setStopText('');
      setTargetText('');
    }
  }

  /** A stop or target tapped on the wrong side of the line: say so, and keep taking taps. */
  const wrongSide = (what: 'stop' | 'target', line: number) => {
    const s = latest.current.side;
    toast({ tone: 'loss', title: `${what === 'stop' ? 'Stop' : 'Target'} not placed there`, body: `A ${s}'s ${what} goes ${(what === 'stop') === (s === 'long') ? 'below' : 'above'} its line at ${perpPrice(line)}.` });
    return true;
  };

  /** A tap's words on the chart: the price, and the P&L there from the line. */
  const tapLabel = (what: string, tapped: number) => (at: number) => {
    const line = lineNow(tapped);
    const coins = coinsOn(line);
    return `${what} · ${perpPrice(at)}${coins != null && coins > 0 ? ` · ${signedUsd(pnlAt(latest.current.side, coins, line, at))}` : ''}`;
  };

  function askTarget(tapped: number) {
    perps.pick(
      k,
      'Target',
      (at) => {
        const line = lineNow(tapped);
        if (!onSide(latest.current.side, 'target', line, at)) return wrongSide('target', line);
        setTargetText(String(at));
        // The line and stop as the fields have them now.
        armLine(line, latest.current.stop, at);
      },
      { step: 'target', color: C.profit, label: tapLabel('Target (optional)', tapped) },
    );
  }

  /** Start "Buy on a line": the next tap on the chart is the line. */
  function startLine() {
    if (amount == null) {
      toast({ tone: 'loss', title: 'Type the amount first', body: sizeIn === 'usd' ? 'How many dollars of margin to put up.' : `How much ${rules.unit}.` });
      return;
    }
    setPriceText('');
    setStopText('');
    setTargetText('');
    perps.pick(
      k,
      'Line',
      (line) => {
        setPriceText(String(line));
        perps.pick(
          k,
          'Stop',
          (at) => {
            if (!onSide(latest.current.side, 'stop', lineNow(line), at)) return wrongSide('stop', lineNow(line));
            setStopText(String(at));
            askTarget(line);
            return true;
          },
          { step: 'stop', color: C.loss, label: tapLabel('Stop (optional)', line) },
        );
        return true;
      },
      { step: 'line', label: (at) => `${latest.current.side === 'long' ? 'Buy' : 'Sell'} on this line · ${perpPrice(at)}` },
    );
  }

  /** Arm now, with the stop and target placed so far. */
  function armNow() {
    const line = typed(priceText);
    if (line == null) return;
    perps.cancelPlacing();
    armLine(roundPrice(k, line), stop, target);
  }

  /** Send the ticket's order — on mainnet once the owner confirms it. */
  function send() {
    if (!ready || size == null) return;
    if (!mainnet) return void sendNow();
    const way = side === 'long' ? 'buy' : 'sell';
    const what = isLine ? 'line order' : kind === 'limit' ? 'limit order' : 'market order';
    setConfirm({
      title: `Send a real ${way} ${what}?`,
      lines: [
        `${sizeText(k, size)} ${perpLabel(k)}${kind === 'market' ? ` at the market (≈ ${perpPrice(price!)}, at most 0.5 % past it)` : ` at ${perpPrice(price!)}`}${reduce ? ', reduce only' : ` at ${usedLev}x isolated`}`,
        ...(reduce ? [] : [`Stop ${stop != null ? perpPrice(stop) : 'none'} · target ${target != null ? perpPrice(target) : 'none'}`]),
        ...(margin != null && margin > 0 ? [`Margin ≈ ${usd(margin)}${liq != null ? ` · liquidation ≈ ${perpPrice(roundPrice(k, liq))}` : ''}`] : []),
      ],
      go: () => void sendNow(),
    });
  }

  async function sendNow() {
    if (size == null) return;
    setSending(true);
    const targets = target != null ? [{ price: target, size: null }] : undefined;
    const levels = reduce || restingAdd ? {} : { stop: stop ?? undefined, targets };
    const way = side === 'long' ? 'buy' : 'sell';
    const ok =
      kind === 'line'
        ? await perps.actions.line({ market: k, side: way, size, leverage: usedLev, price: price!, valid_minutes: live ? null : validity, ...levels })
        : await perps.actions.order({ market: k, side: way, size, leverage: usedLev, type: kind, price: kind === 'limit' ? price! : undefined, reduce_only: reduce || undefined, ...levels });
    setSending(false);
    if (ok) {
      setStopText('');
      setTargetText('');
      if (kind === 'line') setPriceText('');
    }
  }

  const pickButton = (what: string, set: (v: string) => void) => (
    <button type="button" onClick={() => perps.pick(k, what, (p) => set(String(p)))} className="h-8 shrink-0 rounded-6 px-2 type-caption text-accent hover:bg-raised" title="Click the chart to set it">
      Pick
    </button>
  );

  const change = market?.change_pct ?? null;
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 pb-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="num-large text-fg">{mid == null ? '—' : perpPrice(mid)}</span>
            {change != null && <span className={`num-small ${TONE_TEXT[toneOf(change)]}`}>{signedPct(change)}</span>}
          </div>
          <p className="type-caption text-fg-subtle tnum">
            Mark {market?.mark == null ? '—' : perpPrice(market.mark)} · funding {market?.funding_hourly == null ? '—' : `${(market.funding_hourly * 100).toFixed(4)}%/h`}
          </p>
        </div>
        {onClose && (
          <button type="button" onClick={onClose} aria-label="Back to the options" title="Back to the options" className="grid size-8 shrink-0 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg">
            <X size={16} />
          </button>
        )}
      </div>

      <div className="grid grid-cols-2 gap-1 rounded-8 border border-line bg-inset p-1">
        {(['long', 'short'] as const).map((s) => (
          <button
            key={s}
            type="button"
            aria-pressed={side === s}
            onClick={() => setSide(s)}
            className={`${large ? 'h-10' : 'h-8'} rounded-6 type-body-strong transition-colors duration-120 ${side === s ? (s === 'long' ? 'bg-profit text-on-accent' : 'bg-loss text-on-accent') : 'text-fg-muted hover:text-fg'}`}
          >
            {s === 'long' ? 'Long' : 'Short'}
          </button>
        ))}
      </div>
      <button
        type="button"
        role="checkbox"
        aria-checked={isLine}
        onClick={() => {
          if (step) perps.cancelPlacing();
          setKind(isLine ? 'market' : 'line');
        }}
        title="Tap the chart for the line, then its stop and target (both optional); it buys or sells at market once the mark crosses the line"
        className="flex items-center gap-2 self-start rounded-6 px-1 py-1 transition-colors duration-120 hover:bg-raised"
      >
        <span className={`grid size-4 shrink-0 place-items-center rounded-4 border ${isLine ? 'border-accent bg-accent text-on-accent' : 'border-line-strong bg-panel'}`}>{isLine && <Check size={12} strokeWidth={3} />}</span>
        <span className="type-label text-fg">{side === 'long' ? 'Buy' : 'Sell'} on a line</span>
      </button>
      {!isLine && (
        <Segment
          label="Order type"
          size={segSize}
          stretch
          value={kind === 'limit' ? 'limit' : 'market'}
          onChange={(v) => setKind(v)}
          options={[
            { value: 'market', label: 'Market' },
            { value: 'limit', label: 'Limit' },
          ]}
        />
      )}

      {kind !== 'market' && <Field label={kind === 'line' ? 'Line' : 'Limit price'} value={priceText} onChange={setPriceText} unit="$" placeholder={mid == null ? '' : perpPrice(mid)} large={large} action={pickButton(kind === 'line' ? 'Line' : 'Limit price', setPriceText)} />}
      <Field
        label={sizeIn === 'usd' ? 'Amount' : 'Size'}
        value={sizeText_}
        onChange={setSizeText}
        unit={
          <button
            type="button"
            onClick={() => chooseSizeIn(sizeIn === 'usd' ? 'coin' : 'usd')}
            title={sizeIn === 'usd' ? `Dollars of margin, your capital in it. Click to type ${rules.unit} instead` : 'Click to type dollars of margin instead'}
            className="rounded-4 px-1 type-caption text-accent hover:bg-raised"
          >
            {sizeIn === 'usd' ? 'USD' : rules.unit}
          </button>
        }
        placeholder={sizeIn === 'usd' ? '100' : rules.szDecimals > 0 ? `0.${'0'.repeat(rules.szDecimals - 1)}1` : '1'}
        large={large}
      />
      {amount != null && (
        <p className="-mt-2 text-right type-caption text-fg-subtle tnum">
          {sizeIn === 'usd'
            ? size != null && price != null
              ? `Margin · ${sizeText(k, size)}, a ${usd(size * price)} position at ${usedLev}x`
              : isLine
                ? 'Margin · the size follows from the line'
                : kind === 'limit'
                  ? 'Margin · the size follows from the limit price'
                  : 'Margin · the size follows once the price is in'
            : margin != null
              ? `≈ ${usd(margin)} of margin at ${usedLev}x`
              : ''}
        </p>
      )}
      <div className="flex items-center justify-between gap-3">
        <span className="type-label text-fg-muted">Leverage{adding ? ' (the position’s)' : held ? ' (for what opens)' : ''}</span>
        <div role="group" aria-label="Leverage" className="flex items-center rounded-6 border border-line bg-inset">
          <button type="button" aria-label="Less leverage" disabled={adding || lev <= 1} onClick={() => setLeverage(lev - 1)} className="grid size-8 place-items-center text-fg disabled:opacity-40">
            <Minus size={14} />
          </button>
          <span className="w-12 text-center num-medium text-fg">{usedLev}x</span>
          <button type="button" aria-label="More leverage" disabled={adding || lev >= cap} onClick={() => setLeverage(lev + 1)} className="grid size-8 place-items-center text-fg disabled:opacity-40">
            <Plus size={14} />
          </button>
        </div>
      </div>
      {kind === 'line' && !live && (
        <label className="flex items-center justify-between gap-3">
          <span className="type-label text-fg-muted">Valid for</span>
          <select
            value={validity == null ? 'gtc' : String(validity)}
            onChange={(e) => setValidity(e.target.value === 'gtc' ? null : Number(e.target.value))}
            className={`w-[150px] rounded-6 border border-line bg-inset px-2 type-body text-fg ${large ? 'h-11' : 'h-8'}`}
          >
            {VALIDITIES.map((v) => (
              <option key={v.label} value={v.value == null ? 'gtc' : String(v.value)}>
                {v.label}
              </option>
            ))}
          </select>
        </label>
      )}
      {!reduce && !restingAdd && (
        <>
          <Field label="Stop (optional)" value={stopText} onChange={setStopText} unit="$" large={large} action={pickButton('Stop', setStopText)} />
          <Field label="Target (optional)" value={targetText} onChange={setTargetText} unit="$" large={large} action={pickButton('Target', setTargetText)} />
        </>
      )}
      {restingAdd && <p className="type-caption text-fg-subtle">A LIVE {isLine ? 'line order' : 'limit'} that adds to the position takes no stop of its own: once it fills, set the position's stop again and it covers all of it.</p>}
      {kind !== 'line' && (
        <div className="flex items-center justify-between gap-3">
          <span className="type-label text-fg-muted">Reduce only</span>
          <Toggle label="Reduce only" checked={reduceOnly} onChange={setReduceOnly} />
        </div>
      )}

      <p className="type-caption text-fg-subtle tnum">
        {notional == null
          ? `Type ${sizeIn === 'usd' ? 'the dollars of margin' : 'a size'}${kind === 'market' ? '' : isLine ? ', then place the line' : ' and a price'}. At most ${cap}x; a ${rules.unit} size carries ${rules.szDecimals} decimals.`
          : `${usd(notional)} · ${held && !adding ? `closes ${sizeText(k, Math.min(size!, held.size))}${margin! > 0 ? `, then ` : ''}` : ''}${margin! > 0 || !held || adding ? `margin ${usd(margin!)}` : ''} · fee ≈ ${usd(fee!)}${liq != null ? ` · liquidation ≈ ${perpPrice(roundPrice(k, liq))}` : ''}${kind === 'line' ? ` · when the mark ${mid != null && price! > mid ? 'rises' : 'falls'} to ${perpPrice(price!)}` : ''}`}
      </p>
      {live && account && <LiveFunds account={account} k={k} side={side} />}
      {closeOnly && <p className="type-caption text-fg">{perpLabel(k)} is close-only: it was removed from the app's markets. Only orders that reduce your position are taken.</p>}
      {!rules.known && <p className="type-caption text-fg-subtle">{perpLabel(k)} isn't on the app's list of perps.</p>}
      {live && (
        <p className="type-caption text-fg">
          {mainnet
            ? 'LIVE on mainnet: orders go to your Hyperliquid account — real money. Each order that opens asks first; stops and targets rest on Hyperliquid.'
            : 'LIVE on testnet: orders go to your Hyperliquid testnet account (test USDC). Stops and targets rest on Hyperliquid.'}
          {account?.network === 'testnet' && ' Charts and these estimates are mainnet’s; testnet fills at its own prices.'}
        </p>
      )}
      {live && perps.error && <p className="type-caption text-loss">Couldn't refresh the LIVE account ({perps.error}): what is shown is from {account?.as_of ? `${account.as_of.slice(11)} IST` : 'earlier'}.</p>}
      {isLine && step != null ? (
        // Tapping the chart: the line, then the stop, then the target.
        <div className="flex flex-col gap-2">
          <p className="type-caption text-fg">
            {step === 'line' ? 'Tap the chart where the line goes.' : step === 'stop' ? 'Tap the chart for the stop, or arm it without one.' : 'Tap the chart for the target, or arm it without one.'}
          </p>
          <div className="flex gap-2">
            {step !== 'line' && (
              <Button size={size_} kind={side === 'long' ? 'profit' : 'loss'} pointerOnly disabled={sending} onClick={armNow} className="flex-1">
                {sending ? 'Sending…' : 'Arm now'}
              </Button>
            )}
            <Button size={size_} kind="neutral" onClick={() => perps.cancelPlacing()} className={step === 'line' ? 'flex-1' : ''}>
              Cancel
            </Button>
          </div>
        </div>
      ) : isLine && price == null ? (
        <Button size={size_} kind={side === 'long' ? 'profit' : 'loss'} pointerOnly disabled={!rules.known || closeOnly || sending} onClick={startLine}>
          {`${side === 'long' ? 'Buy' : 'Sell'} on a line: tap the chart`}
        </Button>
      ) : (
        <Button size={size_} kind={side === 'long' ? 'profit' : 'loss'} pointerOnly disabled={!ready} onClick={() => void send()}>
          {sending ? 'Sending…' : `${isLine ? 'Arm: ' : ''}${side === 'long' ? 'Buy / long' : 'Sell / short'}${size ? ` ${sizeText(k, size)}` : ''}${kind === 'limit' && price ? ` @ ${perpPrice(price)}` : ''}`}
        </Button>
      )}

      <Modal open={confirm != null} onClose={() => setConfirm(null)} label="Confirm the real order" width="lg:max-w-[440px]">
        <div className="flex flex-col gap-3 p-6">
          <h2 className="type-title text-fg">{confirm?.title}</h2>
          <ul className="flex flex-col gap-1 type-body text-fg-muted tnum">
            {confirm?.lines.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
          <p className="type-caption text-fg-subtle">Hyperliquid mainnet · your own account · real money.</p>
          <div className="mt-2 flex justify-end gap-2">
            <Button size="md" kind="neutral" onClick={() => setConfirm(null)} data-autofocus>
              Cancel
            </Button>
            <Button
              size="md"
              kind="live"
              onClick={() => {
                const go = confirm?.go;
                setConfirm(null);
                go?.();
              }}
            >
              Send it
            </Button>
          </div>
        </div>
      </Modal>

      {account && (
        <section className="flex flex-col gap-1 border-t border-line pt-3">
          <div className="flex items-baseline justify-between">
            <span className="type-label text-fg-muted">{live ? `Hyperliquid account (LIVE · ${account.network})` : 'Perps wallet (paper)'}</span>
            {!live && <span className={`num-medium ${TONE_TEXT[toneOf(account.equity - account.start)]}`}>{signedUsd(account.equity - account.start)}</span>}
          </div>
          <div className="grid grid-cols-2 gap-x-3 gap-y-0.5 type-caption tnum">
            <span className="text-fg-muted">Equity</span>
            <span className="text-right text-fg">{usd(account.equity)}</span>
            <span className="text-fg-muted">Free</span>
            <span className="text-right text-fg">{usd(account.cash)}</span>
            <span className="text-fg-muted">Margin used</span>
            <span className="text-right text-fg">{usd(account.margin)}</span>
            <span className="text-fg-muted">Open P&L</span>
            <span className={`text-right ${TONE_TEXT[toneOf(account.upnl)]}`}>{signedUsd(account.upnl)}</span>
            {(account.venues ?? []).map((v) => (
              <span key={v.dex} className="contents">
                <span className="text-fg-muted">{v.label}</span>
                <span className="text-right text-fg">{usd(v.account_value)}</span>
              </span>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
