import { INDEX_LOT_REFERENCE } from '../../lib/indexLots';
import { LotsStepper } from '../ui/LotsStepper';
import { Section } from './parts';
import { lotsOf } from './summary';
import type { SettingsState } from './useSettings';

/** Lots per index: what each index's Telegram call buys. */
export function Lots({ settings, indexOnly, id, title, large = false }: {
  settings: SettingsState;
  /** `index_options_only` is on. */
  indexOnly: boolean | null;
  id?: string;
  title?: string;
  large?: boolean;
}) {
  const { cfg, save } = settings;
  if (!cfg) return null;
  const lead =
    indexOnly === true
      ? 'Index options only. Calls for anything else are skipped and marked in the Telegram feed.'
      : indexOnly === false
        ? `Stock option calls buy ${cfg.other_lots} ${cfg.other_lots === 1 ? 'lot' : 'lots'} each: Index options only is off (Beta).`
        : undefined;
  return (
    <Section id={id} title={title} lead={lead}>
      <div className={`grid gap-x-6 gap-y-5 p-4 ${large ? 'grid-cols-1' : 'grid-cols-3'}`}>
        {INDEX_LOT_REFERENCE.map((i) => {
          const lots = lotsOf(cfg, i.symbol);
          return (
            <div key={i.symbol} className="flex flex-col gap-2">
              <span className="type-body-strong text-fg">{i.symbol}</span>
              <LotsStepper
                // 0: the index isn't traded; + makes it one lot.
                lots={lots}
                lotSize={i.lotSize}
                size={large ? 'lg' : 'sm'}
                onChange={(n) =>
                  void save((latest) => ({ index_lots_by_symbol: { ...latest.index_lots_by_symbol, [i.symbol]: n } }), `${i.symbol}: ${n} ${n === 1 ? 'lot' : 'lots'}`)
                }
              />
            </div>
          );
        })}
      </div>
    </Section>
  );
}
