import { useState } from 'react';
import type { ExecutePosition } from '../../types';
import { contractLabel } from '../../lib/execute';
import { price, signedMoney } from '../lib/format';
import { Button } from '../ui/Button';
import { LotsStepper } from '../ui/LotsStepper';
import { Modal } from '../ui/Modal';

/** `n` on the `tick` grid, rounded up: a sell never asks less than was
 * typed. The server puts it on the tick the same way. */
function upToTick(n: number, tick: number): number {
  if (!(tick > 0)) return n;
  const steps = n / tick;
  const near = Math.round(steps);
  return Number(((Math.abs(steps - near) < 1e-6 ? near : Math.ceil(steps)) * tick).toFixed(2));
}

/**
 * Asks how much of a held scalp to sell, and at what price or better: the
 * Trade panel's limit close. It only ever closes what is held — the lots go
 * up to the holding — and any other sell of the position cancels it first.
 * LIVE it rests at Kotak as a day order (and can fill in parts); on paper it
 * fills when the last price reaches it. A price at or below the last one
 * sells at once.
 */
export function LimitCloseDialog({ p, tick, live, large, onClose, onPlace }: {
  p: ExecutePosition;
  /** The contract's tick. */
  tick: number;
  /** The owner's LIVE desk: it rests at Kotak. */
  live: boolean;
  /** Phone: 44 pt buttons. */
  large: boolean;
  onClose: () => void;
  /** Send it; resolves `true` once the server took it. */
  onPlace: (lots: number, price: number) => Promise<boolean>;
}) {
  const held = p.lot_size > 0 ? Math.floor(p.qty / p.lot_size) : 0;
  const [lots, setLots] = useState(Math.max(1, held));
  const [text, setText] = useState(p.ltp != null ? upToTick(p.ltp, tick).toFixed(2) : '');
  const [sending, setSending] = useState(false);
  const typed = Number(text.replace(/[,₹\s]/g, ''));
  const valid = text.trim() !== '' && Number.isFinite(typed) && typed > 0;
  const at = valid ? upToTick(typed, tick) : null;
  const qty = lots * p.lot_size;
  const atOnce = at != null && p.ltp != null && at <= p.ltp;
  const pnl = at != null ? (at - p.avg) * qty : null;
  const label = contractLabel(p.index, p.strike, p.option_type);
  const size = large ? 'lg' : 'md';

  async function place() {
    if (at == null || sending || held < 1) return;
    setSending(true);
    const ok = await onPlace(lots, at).catch(() => false);
    setSending(false);
    if (ok) onClose();
  }

  return (
    <Modal open onClose={onClose} label={`Limit sell ${label}`} width="lg:max-w-[440px]" locked={sending}>
      <div className="flex flex-col gap-4 p-6">
        <div>
          <h2 className="type-title text-fg">Limit sell · {label}</h2>
          <p className="mt-1 type-caption text-fg-subtle tnum">
            Holding {held} {held === 1 ? 'lot' : 'lots'} ({p.qty} qty) · avg ₹{price(p.avg)} · last {p.ltp == null ? '—' : `₹${price(p.ltp)}`}
          </p>
        </div>
        <div className="flex items-center justify-between gap-3">
          <span className="type-label text-fg-muted">Quantity</span>
          <LotsStepper lots={lots} lotSize={p.lot_size} onChange={setLots} max={held} size={large ? 'lg' : 'sm'} disabled={sending || held < 1} />
        </div>
        <div className="flex items-center justify-between gap-3">
          <label htmlFor={`limit-price-${p.id}`} className="type-label text-fg-muted">
            Limit price
          </label>
          <span
            className={`flex w-[150px] items-center gap-2 border bg-inset px-3 focus-within:border-accent ${large ? 'h-11 rounded-8' : 'h-9 rounded-6'} ${valid || text === '' ? 'border-line' : 'border-loss'}`}
          >
            <span className="shrink-0 type-caption text-fg-subtle">₹</span>
            <input
              id={`limit-price-${p.id}`}
              data-autofocus
              type="text"
              inputMode="decimal"
              autoComplete="off"
              value={text}
              disabled={sending}
              aria-invalid={!valid}
              onChange={(e) => setText(e.target.value)}
              onFocus={(e) => e.currentTarget.select()}
              className="min-w-0 flex-1 bg-transparent num-medium text-fg outline-none"
            />
          </span>
        </div>
        <p className="type-body text-fg tnum">
          {at == null ? (
            'Type the price to sell at.'
          ) : (
            <>
              Sells {qty} qty at ₹{price(at)} or better
              {pnl != null && (
                <span className={pnl >= 0 ? 'text-profit' : 'text-loss'}>
                  {' '}
                  · {signedMoney(pnl)} on your average
                </span>
              )}
              , before charges.
            </>
          )}
        </p>
        {atOnce && p.ltp != null && (
          <p className="type-caption text-loss">
            At or below the last price (₹{price(p.ltp)}) — it sells at once{live ? ', at the best bids Kotak has' : `, at ₹${price(p.ltp)}`}.
          </p>
        )}
        <p className="type-caption text-fg-subtle">
          {live
            ? 'It rests at Kotak as a day order until it fills, you cancel it, or the market closes, and can fill in parts. '
            : 'It fills when the last price reaches it, and lapses at the close. '}
          Any other sell of this position — Close, −1 lot, a stop or target, exit all — cancels it first, so it only ever sells what you hold.
        </p>
        <div className="mt-1 flex justify-end gap-2">
          <Button size={size} kind="neutral" onClick={onClose} disabled={sending}>
            Cancel
          </Button>
          <Button size={size} kind="loss" pointerOnly disabled={at == null || held < 1 || sending} onClick={() => void place()}>
            {sending ? 'Placing…' : at == null ? 'Place limit sell' : `Sell ${qty} at ₹${price(at)}`}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
