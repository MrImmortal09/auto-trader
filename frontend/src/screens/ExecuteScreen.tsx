import { useEffect, useState } from 'react';
import { LoaderCircle, Zap } from 'lucide-react';
import type { ExecuteBoard, ExecutePosition, Portfolio } from '../types';
import { apiFetch } from '../lib/api';
import { computeAvgBuyPerUnit, getRealizedPnl, todayIsoIST } from '../lib/format';
import {
  buyBlockedReason,
  contractLabel,
  daysLeftLabel,
  expiryLabel,
  fmtSpot,
  INDEX_ORDER,
  MAX_LOTS,
  nearClose,
  readStored,
  writeStored,
} from '../lib/execute';
import { advanceOrders, type Toast, type TrackedOrder } from '../lib/orderTracker';
import { useExecuteStream } from '../hooks/useExecuteStream';
import { useAuth } from '../context/AuthContext';
import { ScalpAlerts, ScalpStatusBar } from '../components/execute/ScalpStatusBar';
import { ScalpPositions } from '../components/execute/ScalpPositions';
import { IndexTabs } from '../components/execute/IndexTabs';
import { LotStepper } from '../components/execute/LotStepper';
import { StrikeLadder } from '../components/execute/StrikeLadder';
import { OrderToasts } from '../components/execute/OrderToasts';

const INDEX_KEY = 'execute.index';
const LOTS_KEY = 'execute.lots';

const clampLots = (n: number) => Math.max(1, Math.min(MAX_LOTS, Math.round(n) || 1));

function without(map: Record<string, boolean>, key: string) {
  const next = { ...map };
  delete next[key];
  return next;
}

function upsert(toasts: Toast[], fresh: Toast[]) {
  const keys = new Set(fresh.map((t) => t.key));
  return [...toasts.filter((t) => !keys.has(t.key)), ...fresh];
}

function BoardInfo({ board }: { board: ExecuteBoard }) {
  const source = { kotak: 'live', upstox: '1-min, Upstox', parity: 'est. from options' };
  return (
    <div className="text-xs text-on-surface-variant flex flex-wrap items-center gap-x-4 gap-y-1">
      <span>
        Expiry <span className="font-bold text-on-surface">{expiryLabel(board.expiry)}</span>{' '}
        <span className={board.days_to_expiry <= 0 ? 'font-bold text-error' : ''}>({daysLeftLabel(board.days_to_expiry)})</span>
      </span>
      <span>
        Lot <span className="font-bold text-on-surface font-mono-code">{board.lot_size}</span>
      </span>
      <span>
        Spot{' '}
        <span className="font-bold text-on-surface font-mono-code tabular-nums">{board.spot != null ? fmtSpot(board.spot) : '—'}</span>
        {board.spot_source && <span className="ml-1 text-[10px]">({source[board.spot_source]})</span>}
      </span>
    </div>
  );
}

/**
 * Scalp Desk — one-tap manual option trading on each index's nearest expiry.
 * Every tap goes to the engine, which places the order with its usual LIVE
 * safeguards; this screen only shows prices and follows the outcome.
 */
export function ExecuteScreen({ serverBase, portfolio }: { serverBase: string; portfolio: Portfolio | null }) {
  const { hasWriteAccess, openUnlockModal } = useAuth();
  const [index, setIndex] = useState<string>(() => {
    const saved = readStored(INDEX_KEY, 'NIFTY');
    return INDEX_ORDER.includes(saved) ? saved : 'NIFTY';
  });
  const [lotsByIndex, setLotsByIndex] = useState<Record<string, number>>(() => readStored(LOTS_KEY, {}));
  const { frame, stale, now } = useExecuteStream(serverBase, index);

  const [pendingSyms, setPendingSyms] = useState<Record<string, boolean>>({});
  const [busyIds, setBusyIds] = useState<Record<string, boolean>>({});
  const [exitAllBusy, setExitAllBusy] = useState(false);
  const [orders, setOrders] = useState<TrackedOrder[]>([]);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [seenTs, setSeenTs] = useState(0);

  // Follow every tap through each new frame until it fills, fails or sells.
  if (frame && frame.ts !== seenTs) {
    setSeenTs(frame.ts);
    if (orders.length > 0) {
      const { orders: next, toasts: fresh } = advanceOrders(orders, frame);
      setOrders(next);
      if (fresh.length > 0) setToasts((ts) => upsert(ts, fresh));
    }
  }

  const lots = clampLots(lotsByIndex[index] ?? 1);
  const board = frame?.board && frame.board.index === index ? frame.board : null;
  const positions = frame?.positions ?? [];
  const manual = positions.filter((p) => p.manual);
  const heldBySymbol = new Map(manual.map((p) => [p.symbol, p]));
  const blocked = buyBlockedReason(frame, stale);
  const frameMs = frame?.ts ?? now;

  const trades = portfolio?.trades ?? [];
  const avgBuy = computeAvgBuyPerUnit(trades);
  const today = todayIsoIST();
  const realized = trades
    .filter((t) => t.signal_id?.startsWith('manual-') && t.timestamp.startsWith(today))
    .reduce((acc, t) => acc + getRealizedPnl(t, avgBuy), 0);
  const openPnl = manual.reduce((acc, p) => acc + (p.pnl ?? 0), 0);

  function selectIndex(next: string) {
    setIndex(next);
    writeStored(INDEX_KEY, next);
  }

  function changeLots(next: number) {
    const updated = { ...lotsByIndex, [index]: clampLots(next) };
    setLotsByIndex(updated);
    writeStored(LOTS_KEY, updated);
  }

  // 1–6 switch index, + / − change lots. No key ever sends an order.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const el = e.target as HTMLElement | null;
      if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable)) return;
      const n = Number(e.key);
      if (Number.isInteger(n) && n >= 1 && n <= INDEX_ORDER.length) {
        setIndex(INDEX_ORDER[n - 1]);
        writeStored(INDEX_KEY, INDEX_ORDER[n - 1]);
      } else if (e.key === '+' || e.key === '=' || e.key === '-' || e.key === '_') {
        const delta = e.key === '+' || e.key === '=' ? 1 : -1;
        const updated = { ...lotsByIndex, [index]: clampLots((lotsByIndex[index] ?? 1) + delta) };
        setLotsByIndex(updated);
        writeStored(LOTS_KEY, updated);
      }
    }
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [index, lotsByIndex]);

  function toast(t: Toast) {
    setToasts((ts) => upsert(ts, [t]));
  }

  function dismissToast(key: string) {
    setToasts((ts) => ts.filter((t) => t.key !== key));
  }

  function track(order: Omit<TrackedOrder, 'seen' | 'lastPnl'>) {
    setOrders((os) => [...os.filter((o) => o.key !== order.key), { ...order, seen: false, lastPnl: null }]);
  }

  function needsWriteAccess(reason: string) {
    if (hasWriteAccess) return false;
    openUnlockModal(reason);
    return true;
  }

  async function buy(sym: string, label: string) {
    if (needsWriteAccess('Placing orders requires Write Access')) return;
    if (!frame || !board || blocked || pendingSyms[sym]) return;
    const held = heldBySymbol.get(sym);
    const key = `buy-${sym}-${Date.now()}`;
    const qty = lots * board.lot_size;
    setPendingSyms((s) => ({ ...s, [sym]: true }));
    toast({ key, tone: 'pending', title: `${held ? 'Adding' : 'Buying'} ${qty} ${label}`, detail: 'Market order sent to the engine…' });
    try {
      const res = await apiFetch(serverBase, '/api/execute/buy', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ trading_symbol: sym, lots }),
      });
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        toast({ key, tone: 'err', title: `${label} not sent`, detail: body?.error ?? `Server answered ${res.status}` });
        return;
      }
      track({
        key,
        kind: body?.action === 'added' ? 'add' : 'buy',
        positionId: String(body?.position_id ?? ''),
        label,
        index: board.index,
        baseQty: held?.qty ?? 0,
        startTs: frame.ts,
      });
    } catch {
      toast({ key, tone: 'err', title: `${label}: no answer from the server`, detail: 'It may or may not have gone through — check Open positions before tapping again.' });
    } finally {
      setPendingSyms((s) => without(s, sym));
    }
  }

  async function sell(p: ExecutePosition, qty: number | null) {
    if (needsWriteAccess('Selling requires Write Access')) return;
    if (!frame || busyIds[p.id]) return;
    const label = contractLabel(p.index, p.strike, p.option_type);
    const key = `${qty == null ? 'exit' : 'sell'}-${p.id}-${Date.now()}`;
    setBusyIds((b) => ({ ...b, [p.id]: true }));
    toast({ key, tone: 'pending', title: qty == null ? `Exiting ${label}` : `Selling ${qty} ${label}`, detail: 'Market sell sent to the engine…' });
    try {
      const res = await apiFetch(
        serverBase,
        qty == null ? `/api/positions/${p.id}/close` : `/api/positions/${p.id}/sell`,
        qty == null
          ? { method: 'POST' }
          : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ qty }) },
      );
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        toast({ key, tone: 'err', title: `${label} not sold`, detail: body?.error ?? `Server answered ${res.status}` });
        return;
      }
      track({ key, kind: qty == null ? 'exit' : 'sell', positionId: p.id, label, index: p.index, baseQty: p.qty, startTs: frame.ts });
    } catch {
      toast({ key, tone: 'err', title: `${label}: no answer from the server`, detail: 'Check Open positions before trying again.' });
    } finally {
      setBusyIds((b) => without(b, p.id));
    }
  }

  async function exitAll() {
    if (needsWriteAccess('Exiting requires Write Access')) return;
    if (!frame) return;
    const targets = manual.filter((p) => !p.exiting);
    const startTs = frame.ts;
    setExitAllBusy(true);
    try {
      const res = await apiFetch(serverBase, '/api/execute/exit-all', { method: 'POST' });
      const body = await res.json().catch(() => null);
      if (!res.ok) {
        toast({ key: `exit-all-${Date.now()}`, tone: 'err', title: 'Exit all not sent', detail: body?.error ?? `Server answered ${res.status}` });
        return;
      }
      for (const p of targets) {
        const label = contractLabel(p.index, p.strike, p.option_type);
        const key = `exit-${p.id}-${startTs}`;
        toast({ key, tone: 'pending', title: `Exiting ${label}`, detail: 'Market sell sent to the engine…' });
        track({ key, kind: 'exit', positionId: p.id, label, index: p.index, baseQty: p.qty, startTs });
      }
    } catch {
      toast({ key: `exit-all-${Date.now()}`, tone: 'err', title: 'Exit all: no answer from the server', detail: 'Check Open positions — tap EXIT on each if they are still there.' });
    } finally {
      setExitAllBusy(false);
    }
  }

  return (
    <div className="max-w-6xl mx-auto space-y-3 sm:space-y-4">
      <div className="sticky top-0 z-30 space-y-2">
        <ScalpStatusBar
          frame={frame}
          streamStale={stale}
          portfolio={portfolio}
          realized={realized}
          open={openPnl}
          manualOpen={manual.length}
          onExitAll={exitAll}
          exitAllBusy={exitAllBusy}
        />
        <ScalpAlerts frame={frame} streamStale={stale} />
      </div>

      <ScalpPositions
        positions={positions}
        busyIds={busyIds}
        nearClose={nearClose(frameMs)}
        onExit={(p) => sell(p, null)}
        onSellLot={(p) => sell(p, p.lot_size)}
      />

      <section className="rounded-xl border border-outline-variant bg-surface-container-lowest shadow-sm overflow-hidden">
        <div className="p-3 sm:p-4 space-y-3 border-b border-outline-variant">
          <IndexTabs frame={frame} active={index} onSelect={selectIndex} />
          <div className="flex flex-wrap items-center justify-between gap-3">
            {board ? <BoardInfo board={board} /> : <span className="text-xs text-on-surface-variant">{index}</span>}
            <LotStepper lots={lots} lotSize={board?.lot_size ?? null} onChange={changeLots} />
          </div>
        </div>

        {board ? (
          <StrikeLadder
            key={board.index}
            board={board}
            nowMs={now}
            heldBySymbol={heldBySymbol}
            pendingSyms={pendingSyms}
            busyIds={busyIds}
            blocked={blocked}
            onBuy={buy}
            onExit={(p) => sell(p, null)}
            onShiftedTap={() =>
              toast({ key: `shift-${Date.now()}`, tone: 'err', title: 'Ladder just moved — nothing sent', detail: 'Check the strike and tap again.' })
            }
          />
        ) : (
          <div className="px-4 py-12 flex flex-col items-center gap-2 text-sm text-on-surface-variant">
            {frame?.board_error ? (
              <span>{frame.board_error}</span>
            ) : (
              <>
                <LoaderCircle size={20} className="animate-spin text-primary" />
                <span>Loading the {index} ladder…</span>
              </>
            )}
          </div>
        )}

        <div className="px-4 py-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[10px] text-on-surface-variant bg-surface-container-low border-t border-outline-variant">
          <span className="inline-flex items-center gap-1 font-semibold text-on-surface">
            <Zap size={11} className="text-secondary" />One tap = market order, no confirm
          </span>
          <span>No stop, no target — exit by hand</span>
          <span>NRML: carries overnight if left open</span>
          <span className="inline-flex items-center gap-1">
            <span className="w-2.5 h-2.5 rounded-sm bg-amber-50 border border-amber-200" />in the money
          </span>
        </div>
      </section>

      <OrderToasts toasts={toasts} onDismiss={dismissToast} />
    </div>
  );
}
