import { NumberInput } from '../ui/NumberInput';
import { Toggle } from '../ui/Toggle';
import { Row, Section } from './parts';
import type { SettingsState } from './useSettings';

/**
 * Exits: what target 1 sells, the dynamic target ladder after it, and
 * protecting gains before it. Each change saves at once.
 */
export function Exits({ settings, id, title, large = false }: { settings: SettingsState; id?: string; title?: string; large?: boolean }) {
  const { cfg, save } = settings;
  if (!cfg) return null;
  const size = large ? 'lg' : 'md';
  const dynamic = cfg.dynamic_targeting;
  const protect = cfg.pre_t1_trailing;
  return (
    <Section id={id} title={title}>
      <Row title="Target 1" caption={dynamic ? 'Share sold when target 1 is hit. With dynamic targets on, target 1 sells one lot instead.' : 'Share sold when target 1 is hit'}>
        <NumberInput label="Target 1" value={cfg.target_1_exit_pct} min={0} max={100} unit="%" size={size} onCommit={(n) => void save(() => ({ target_1_exit_pct: n }), `Target 1 sells ${n}%`)} />
      </Row>
      <Row title="Dynamic targets" caption="Keep a lot running after T1 and move the stop up">
        <Toggle label="Dynamic targets" checked={dynamic} onChange={(on) => void save(() => ({ dynamic_targeting: on }), `Dynamic targets ${on ? 'on' : 'off'}`)} />
      </Row>
      <Row indent title="Trail factor" caption="0 locks each target · 1 trails to breakeven">
        <NumberInput
          label="Trail factor"
          value={cfg.dynamic_targeting_trail_factor}
          min={0}
          max={1}
          decimals={1}
          unit="×"
          size={size}
          disabled={!dynamic}
          onCommit={(n) => void save(() => ({ dynamic_targeting_trail_factor: n }), `Trail factor ${n}`)}
        />
      </Row>
      <Row indent title="Extension factor" caption="Next target distance, as a share of entry to T1">
        <NumberInput
          label="Extension factor"
          value={cfg.dynamic_targeting_extension_factor}
          min={0.05}
          max={5}
          decimals={1}
          unit="×"
          size={size}
          disabled={!dynamic}
          onCommit={(n) => void save(() => ({ dynamic_targeting_extension_factor: n }), `Extension factor ${n}`)}
        />
      </Row>
      <Row indent title="Single-lot positions too" caption="Ride targets instead of exiting at T1">
        <Toggle
          label="Single-lot positions too"
          checked={cfg.dynamic_targeting_single_lot}
          disabled={!dynamic}
          onChange={(on) => void save(() => ({ dynamic_targeting_single_lot: on }), `Single-lot dynamic targets ${on ? 'on' : 'off'}`)}
        />
      </Row>
      <Row title="Protect gains before T1" caption="Trail the stop below the peak before T1">
        <Toggle label="Protect gains before T1" checked={protect} onChange={(on) => void save(() => ({ pre_t1_trailing: on }), `Protect gains before T1 ${on ? 'on' : 'off'}`)} />
      </Row>
      <Row indent title="Arm at" caption="Share of entry to T1 covered first">
        <NumberInput
          label="Arm at"
          value={cfg.pre_t1_trail_arm_pct}
          min={0}
          max={100}
          unit="%"
          size={size}
          disabled={!protect}
          onCommit={(n) => void save(() => ({ pre_t1_trail_arm_pct: n }), `Arm at ${n}%`)}
        />
      </Row>
    </Section>
  );
}
