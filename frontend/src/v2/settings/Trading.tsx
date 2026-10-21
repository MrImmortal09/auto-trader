import { useState } from 'react';
import { CircleAlert } from 'lucide-react';
import { getModeCheck } from '../lib/api';
import { useToast } from '../ui/toast';
import { Button } from '../ui/Button';
import { NumberInput } from '../ui/NumberInput';
import { Segment } from '../ui/Segment';
import { LiveDialog } from './LiveDialog';
import { paperBlockedReason } from './mode';
import { Row, Section } from './parts';
import type { SettingsState } from './useSettings';

/**
 * Trading: Paper or LIVE (going LIVE is typed), the stock-call budget and
 * the kill switch.
 */
export function Trading({ serverBase, settings, onKillSwitch, onModeChanged, id, title, large = false }: {
  serverBase: string;
  settings: SettingsState;
  onKillSwitch: () => void;
  /** After a switch between Paper and LIVE: refresh what shows the mode. */
  onModeChanged: () => void;
  id?: string;
  title?: string;
  /** Phone: 44 pt controls. */
  large?: boolean;
}) {
  const { cfg, save } = settings;
  const toast = useToast();
  const [liveOpen, setLiveOpen] = useState(false);
  const [checking, setChecking] = useState(false);
  if (!cfg) return null;
  const mode = cfg.mode === 'LIVE' ? 'LIVE' : 'PAPER';

  async function toPaper() {
    setChecking(true);
    try {
      const check = await getModeCheck(serverBase);
      const why = paperBlockedReason(check);
      if (why) {
        toast({ tone: 'loss', title: "Still LIVE", body: why });
        return;
      }
      if (await save(() => ({ mode: 'PAPER' }), 'Mode: Paper')) onModeChanged();
    } catch {
      toast({ tone: 'loss', title: "Couldn't check what is open", body: 'Nothing changed.' });
    } finally {
      setChecking(false);
    }
  }

  return (
    <Section id={id} title={title}>
      <Row title="Mode" caption="LIVE sends real orders through Kotak. Switching to LIVE asks you to type LIVE.">
        <Segment
          label="Mode"
          size={large ? 'lg' : 'sm'}
          value={mode}
          onChange={(m) => {
            if (m === mode || checking) return;
            if (m === 'LIVE') setLiveOpen(true);
            else void toPaper();
          }}
          options={[
            { value: 'PAPER', label: 'Paper' },
            { value: 'LIVE', label: 'LIVE' },
          ]}
        />
      </Row>
      <Row title="Capital per trade" caption="Sizes stock calls only: quantity is this amount over the price, in whole lots. Option calls buy the lots per index below.">
        <NumberInput
          label="Capital per trade"
          value={cfg.max_trade_amount_inr}
          min={0}
          max={10_000_000}
          grouped
          unit="₹"
          size={large ? 'lg' : 'md'}
          onCommit={(n) => void save(() => ({ max_trade_amount_inr: n }), `Capital per trade: ₹${n.toLocaleString('en-IN')}`)}
        />
      </Row>
      <Row title="Kill switch" caption="Stops new entries for the day and exits every open position at market.">
        {cfg.kill_switch_active ? (
          <span className="inline-flex h-8 items-center gap-1.5 rounded-6 border border-loss px-3 type-button text-loss">
            <CircleAlert size={14} /> Trading stopped
          </span>
        ) : (
          <Button size={large ? 'lg' : 'md'} kind="alert" icon={<CircleAlert size={14} />} onClick={onKillSwitch}>
            Kill switch
          </Button>
        )}
      </Row>
      {liveOpen && (
        <LiveDialog
          serverBase={serverBase}
          onClose={() => setLiveOpen(false)}
          onConfirm={async () => {
            const ok = await save(() => ({ mode: 'LIVE' }), 'Mode: LIVE');
            if (ok) onModeChanged();
            return ok;
          }}
        />
      )}
    </Section>
  );
}
