import { useState, type ReactNode } from 'react';
import { Minus, Plus, X } from 'lucide-react';
import type { HlMarket, PerpAccount, PerpSide } from '../../lib/types';
import { signedPct, toneOf } from '../../lib/format';
import { Button } from '../../ui/Button';
import { Segment } from '../../ui/Segment';
import { Toggle } from '../../ui/Toggle';
import type { PerpKey } from '../model';
import { estLiq, leverageCap, PERP_RULES, perpPrice, roundPrice, roundSize, signedUsd, sizeText, usd, VALIDITIES } from './model';
import type { Perps } from './usePerps';

type OrderKind = 'market' | 'limit' | 'line';

const TONE_TEXT = { profit: 'text-profit', loss: 'text-loss', neutral: 'text-fg' } as const;

/** A number typed into a field, or null when it isn't one. */
function typed(text: string): number | null {
  const n = Number(text.replace(/[,$\s]/g, ''));
  return text.trim() !== '' && Number.isFinite(n) && n > 0 ? n : null;
}

/** An inset field with its unit. */
function Field({ label, value, onChange, unit, placeholder, large, action }: { label: string; value: string; onChange: (v: string) => void; unit?: string; placeholder?: string; large: boolean; action?: ReactNode }) {
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
 * The perp order ticket, in the options ladder's place while a perp is
 * picked: long or short, market, limit or line order, a size in the coin,
 * leverage up to the cap, and an optional stop and target the position
 * takes. Paper only for now: it fills on the live Hyperliquid market. Under
 * it, the account: equity, free USDC, margin and open P&L.
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

  const rules = PERP_RULES[k];
  const cap = leverageCap(k);
  const lev = Math.max(1, Math.min(cap, leverage));
  const mid = market?.mid ?? market?.mark ?? null;
  const held = account?.positions.find((p) => p.market === k) ?? null;
  // An add puts margin up at the position's own leverage; an order the other
  // way first closes what is held, and opens what is left over at the
  // stepper's — the leverage shown is the one sent.
  const adding = held != null && held.side === side;
  const usedLev = adding ? held.leverage : lev;
  const size = typed(sizeText_) == null ? null : roundSize(k, typed(sizeText_)!);
  const price = kind === 'market' ? mid : typed(priceText) == null ? null : roundPrice(k, typed(priceText)!);
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
  // LIVE is read-only in this release: orders stay on paper.
  const live = account?.mode === 'live';
  const ready = !live && size != null && size > 0 && price != null && !sending;
  const size_ = large ? 'lg' : 'md';
  const segSize = large ? 'lg' : 'sm';

  async function send() {
    if (!ready || size == null) return;
    setSending(true);
    const targets = target != null ? [{ price: target, size: null }] : undefined;
    const levels = reduce ? {} : { stop: stop ?? undefined, targets };
    const way = side === 'long' ? 'buy' : 'sell';
    const ok =
      kind === 'line'
        ? await perps.actions.line({ market: k, side: way, size, leverage: usedLev, price: price!, valid_minutes: validity, ...levels })
        : await perps.actions.order({ market: k, side: way, size, leverage: usedLev, type: kind, price: kind === 'limit' ? price! : undefined, reduce_only: reduce || undefined, ...levels });
    setSending(false);
    if (ok) {
      setStopText('');
      setTargetText('');
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
      <Segment
        label="Order type"
        size={segSize}
        stretch
        value={kind}
        onChange={(v) => setKind(v)}
        options={[
          { value: 'market', label: 'Market' },
          { value: 'limit', label: 'Limit' },
          { value: 'line', label: 'Line' },
        ]}
      />

      {kind !== 'market' && <Field label={kind === 'line' ? 'Line' : 'Limit price'} value={priceText} onChange={setPriceText} unit="$" placeholder={mid == null ? '' : perpPrice(mid)} large={large} action={pickButton(kind === 'line' ? 'Line' : 'Limit price', setPriceText)} />}
      <Field label="Size" value={sizeText_} onChange={setSizeText} unit={k} placeholder={`0.${'0'.repeat(rules.szDecimals - 1)}1`} large={large} />
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
      {kind === 'line' && (
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
      {!reduce && (
        <>
          <Field label="Stop (optional)" value={stopText} onChange={setStopText} unit="$" large={large} action={pickButton('Stop', setStopText)} />
          <Field label="Target (optional)" value={targetText} onChange={setTargetText} unit="$" large={large} action={pickButton('Target', setTargetText)} />
        </>
      )}
      {kind !== 'line' && (
        <div className="flex items-center justify-between gap-3">
          <span className="type-label text-fg-muted">Reduce only</span>
          <Toggle label="Reduce only" checked={reduceOnly} onChange={setReduceOnly} />
        </div>
      )}

      <p className="type-caption text-fg-subtle tnum">
        {notional == null
          ? `Type a size${kind === 'market' ? '' : ' and a price'}. At most ${cap}x; a ${k} size carries ${rules.szDecimals} decimals.`
          : `${usd(notional)} · ${held && !adding ? `closes ${sizeText(k, Math.min(size!, held.size))}${margin! > 0 ? `, then ` : ''}` : ''}${margin! > 0 || !held || adding ? `margin ${usd(margin!)}` : ''} · fee ≈ ${usd(fee!)}${liq != null ? ` · liquidation ≈ ${perpPrice(roundPrice(k, liq))}` : ''}${kind === 'line' ? ` · when the mark ${mid != null && price! > mid ? 'rises' : 'falls'} to ${perpPrice(price!)}` : ''}`}
      </p>
      {live && (
        <p className="type-caption text-fg">
          LIVE on {account?.network}: your Hyperliquid account is shown read-only. LIVE orders come in the next release; switch Hyperliquid to Paper in Settings to trade on paper.
          {account?.network === 'testnet' && ' Charts, prices and these estimates are mainnet’s; testnet’s own prices differ.'}
        </p>
      )}
      {live && perps.error && <p className="type-caption text-loss">Couldn't refresh the LIVE account ({perps.error}): what is shown is from {account?.as_of ? `${account.as_of.slice(11)} IST` : 'earlier'}.</p>}
      <Button size={size_} kind={side === 'long' ? 'profit' : 'loss'} pointerOnly disabled={!ready} onClick={() => void send()}>
        {sending ? 'Sending…' : `${kind === 'line' ? 'Arm: ' : ''}${side === 'long' ? 'Buy / long' : 'Sell / short'}${size ? ` ${sizeText(k, size)}` : ''}${kind === 'limit' && price ? ` @ ${perpPrice(price)}` : ''}`}
      </Button>

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
