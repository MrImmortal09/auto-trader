import type { TopupMode } from '../../types';
import { NumberInput } from '../ui/NumberInput';
import { Segment } from '../ui/Segment';
import { Toggle } from '../ui/Toggle';
import { Row, Section } from './parts';
import type { SettingsState } from './useSettings';

const TOPUP_RULES: { value: TopupMode; label: string }[] = [
  { value: 'strong_to_agree', label: 'Strong agree, to the Agrees size' },
  { value: 'strong_to_strong', label: 'Strong agree, to the Strong size' },
  { value: 'tier', label: 'Any agree, to its own size' },
  { value: 'step', label: 'Step up, twice at most' },
];

const INTERVALS = ['5', '15', '30', '60'];
const WINDOWS = [
  { value: '5', label: 'ATM ±5' },
  { value: '10', label: '±10' },
  { value: '20', label: '±20' },
  { value: '0', label: 'All' },
];

/** Telegram calls: how long one waits, where it may be bought, and stock calls. */
export function TelegramCalls({ settings, indexOnly, id, title, large = false }: { settings: SettingsState; indexOnly: boolean | null; id?: string; title?: string; large?: boolean }) {
  const { cfg, save } = settings;
  if (!cfg) return null;
  const size = large ? 'lg' : 'md';
  return (
    <Section id={id} title={title}>
      <Row title="Call validity" caption="A call not bought within this many minutes is dropped. 0 keeps it until the 15:39 cutoff.">
        <NumberInput label="Call validity" value={cfg.call_validity_minutes} min={0} max={390} unit="min" size={size} onCommit={(n) => void save(() => ({ call_validity_minutes: Math.round(n) }), `Calls valid ${Math.round(n)} min`)} />
      </Row>
      <Row title="Entry buy window" caption="Buy only this far past the trigger, as a share of entry to T1. 0 buys the moment it triggers.">
        <NumberInput label="Entry buy window" value={cfg.entry_window_pct} min={0} max={100} unit="%" size={size} onCommit={(n) => void save(() => ({ entry_window_pct: n }), `Entry buy window ${n}%`)} />
      </Row>
      <Row title="Entry price protection" caption="A LIVE entry buy fills at most this much above its trigger price.">
        <NumberInput
          label="Entry price protection"
          value={cfg.entry_market_protection}
          min={0}
          max={20}
          decimals={1}
          unit="%"
          size={size}
          onCommit={(n) => void save(() => ({ entry_market_protection: n }), `Entry price protection ${n}%`)}
        />
      </Row>
      <Row title="Stock option calls" caption={indexOnly ? 'Not traded: Index options only is on (Beta).' : 'Lots a stock option call buys. 0 skips them.'}>
        <NumberInput label="Stock option lots" value={cfg.other_lots} min={0} max={100} unit="lots" size={size} disabled={indexOnly === true} onCommit={(n) => void save(() => ({ other_lots: Math.round(n) }), `Stock option calls buy ${Math.round(n)} lots`)} />
      </Row>
    </Section>
  );
}

/** Indicator sizing: whether the VWAP + PCR check sizes a call, by how
 * much, and how a check is scored (which Market shows too). */
export function IndicatorSizing({ settings, id, title, large = false }: { settings: SettingsState; id?: string; title?: string; large?: boolean }) {
  const { cfg, save } = settings;
  if (!cfg) return null;
  const size = large ? 'lg' : 'md';
  const on = cfg.indicator_sizing;
  const x = (label: string, key: 'indicator_strong_agree_multiplier' | 'indicator_agree_multiplier' | 'indicator_neutral_multiplier', caption: string) => (
    <Row indent title={label} caption={caption}>
      <NumberInput label={label} value={cfg[key]} min={0.1} max={5} decimals={1} unit="×" size={size} disabled={!on} onCommit={(n) => void save(() => ({ [key]: n }), `${label} ×${n}`)} />
    </Row>
  );
  return (
    <div className="flex flex-col gap-4">
      <Section id={id} title={title}>
        <Row title="Size calls on the indicator" caption="When a call triggers, read its index's last closed check: agreeing buys more, neutral less, disagreeing doesn't buy.">
          <Toggle label="Size calls on the indicator" checked={on} onChange={(v) => void save(() => ({ indicator_sizing: v }), `Indicator sizing ${v ? 'on' : 'off'}`)} />
        </Row>
        <Row indent title="When it disagrees" caption="Keep the call waiting until it agrees, or drop it.">
          <Segment
            label="When it disagrees"
            value={cfg.indicator_wait_on_disagree ? 'wait' : 'drop'}
            disabled={!on}
            onChange={(v) => void save(() => ({ indicator_wait_on_disagree: v === 'wait' }), v === 'wait' ? 'Disagreeing calls wait' : 'Disagreeing calls are dropped')}
            options={[
              { value: 'wait', label: 'Wait' },
              { value: 'drop', label: 'Drop' },
            ]}
          />
        </Row>
        {x('Strong agree', 'indicator_strong_agree_multiplier', '+2 for CE, −2 for PE: PCR and VWAP both on its side')}
        {x('Agrees', 'indicator_agree_multiplier', 'One of them on its side, the other neutral')}
        {x('Neutral', 'indicator_neutral_multiplier', 'Neutral, or no fresh check')}
        <Row indent title="Top up when it agrees" caption="A call bought at the neutral size buys more when the indicator agrees later, before T1.">
          <Toggle label="Top up when it agrees" checked={cfg.indicator_topup} disabled={!on} onChange={(v) => void save(() => ({ indicator_topup: v }), `Top-up ${v ? 'on' : 'off'}`)} />
        </Row>
        <Row indent stacked={large} title="Top-up rule" caption="Which reading tops up, and to which size">
          <select
            aria-label="Top-up rule"
            value={cfg.indicator_topup_mode}
            disabled={!on || !cfg.indicator_topup}
            onChange={(e) => void save(() => ({ indicator_topup_mode: e.target.value as TopupMode }), `Top-up rule: ${TOPUP_RULES.find((r) => r.value === e.target.value)?.label}`)}
            className={`rounded-6 border border-line bg-inset px-2.5 type-body text-fg outline-none focus:border-line-strong disabled:opacity-50 ${large ? 'h-11' : 'h-9'}`}
          >
            {TOPUP_RULES.map((r) => (
              <option key={r.value} value={r.value}>
                {r.label}
              </option>
            ))}
          </select>
        </Row>
        <Row title="Lower a stop only on a strong agree" caption="A channel moving a stop down applies only on +2 for CE or −2 for PE. Moving it up always applies.">
          <Toggle label="Lower a stop only on a strong agree" checked={cfg.indicator_sl_lower_gate} onChange={(v) => void save(() => ({ indicator_sl_lower_gate: v }), `Lower-stop check ${v ? 'on' : 'off'}`)} />
        </Row>
      </Section>
      <Section lead="How a check is scored: score = option signal + VWAP signal. Market shows the same.">
        <Row title="Checks every" caption="Read one minute after each boundary; the first at 09:16">
          <Segment label="Checks every" value={String(cfg.indicator_interval_min)} onChange={(v) => void save(() => ({ indicator_interval_min: Number(v) }), `Checks every ${v} min`)} options={INTERVALS.map((m) => ({ value: m, label: `${m} min` }))} />
        </Row>
        <Row title="Opening checks" caption="09:16 reads neutral, then 5-minute checks until the first regular one">
          <Toggle label="Opening checks" checked={cfg.indicator_early_checks} onChange={(v) => void save(() => ({ indicator_early_checks: v }), `Opening checks ${v ? 'on' : 'off'}`)} />
        </Row>
        <Row title="Strikes" caption="Strikes each side of ATM that count">
          <Segment label="Strikes" value={String(cfg.indicator_strike_window)} onChange={(v) => void save(() => ({ indicator_strike_window: Number(v) }), `Strikes: ${WINDOWS.find((w) => w.value === v)?.label}`)} options={WINDOWS} />
        </Row>
        <Row title="Basis" caption="Change since yesterday's close, or total open interest">
          <Segment
            label="Basis"
            value={cfg.indicator_basis}
            onChange={(v) => void save(() => ({ indicator_basis: v }), v === 'chg' ? 'Basis: change in OI' : 'Basis: total OI')}
            options={[
              { value: 'chg', label: 'Change in OI' },
              { value: 'total', label: 'Total OI' },
            ]}
          />
        </Row>
        <Row title="PCR buy above" caption="The option signal buys above this">
          <NumberInput
            label="PCR buy above"
            value={cfg.indicator_pcr_buy_above}
            min={0}
            max={10}
            decimals={2}
            size={size}
            onCommit={(n) => void save((l) => ({ indicator_pcr_buy_above: n, indicator_pcr_sell_below: Math.min(l.indicator_pcr_sell_below, n) }), `PCR buy above ${n.toFixed(2)}`)}
          />
        </Row>
        <Row title="PCR sell below" caption="The option signal sells below this; never above the buy line">
          <NumberInput label="PCR sell below" value={cfg.indicator_pcr_sell_below} min={0} max={cfg.indicator_pcr_buy_above} decimals={2} size={size} onCommit={(n) => void save(() => ({ indicator_pcr_sell_below: n }), `PCR sell below ${n.toFixed(2)}`)} />
        </Row>
        <Row title="VWAP band" caption="The VWAP signal is neutral while the future is this close to its VWAP">
          <NumberInput label="VWAP band" value={cfg.indicator_vwap_band_pct} min={0} max={5} decimals={2} unit="%" size={size} onCommit={(n) => void save(() => ({ indicator_vwap_band_pct: n }), `VWAP band ${n}%`)} />
        </Row>
      </Section>
    </div>
  );
}
