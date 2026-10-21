// The phone's Settings list: one line under each section saying how it's set.

import type { TradingConfig } from '../../types';
import { INDEX_LOT_REFERENCE } from '../../lib/indexLots';
import type { Flags, HealthSet, SystemAnswer } from '../lib/types';

/** What a call on `index` buys: its own lots, else the default for indices. */
export function lotsOf(cfg: TradingConfig, index: string): number {
  const own = cfg.index_lots_by_symbol?.[index];
  return own != null && own > 0 ? own : cfg.index_lots;
}

/** "Kotak, Telegram and Upstox connected", "Kotak not connected". */
export function connectionsLine(health: HealthSet | null): string {
  if (!health) return 'Checking…';
  const names: [keyof HealthSet, string][] = [
    ['kotak', 'Kotak'],
    ['telegram', 'Telegram'],
    ['upstox', 'Upstox'],
  ];
  const down = names.filter(([k]) => health[k].level === 'down').map(([, n]) => n);
  if (down.length > 0) return `${down.join(' and ')} not connected`;
  const up = names.filter(([k]) => health[k].level === 'ok').map(([, n]) => n);
  if (up.length === 0) return 'Nothing connected';
  return `${up.length > 1 ? `${up.slice(0, -1).join(', ')} and ${up[up.length - 1]}` : up[0]} connected`;
}

export function tradingLine(cfg: TradingConfig): string {
  return `${cfg.mode === 'LIVE' ? 'LIVE' : 'Paper'} · ${cfg.kill_switch_active ? 'trading stopped' : 'kill switch off'}`;
}

export function lotsLine(cfg: TradingConfig): string {
  return INDEX_LOT_REFERENCE.map((i) => `${i.symbol} ${lotsOf(cfg, i.symbol)}`).join(' · ');
}

export function exitsLine(cfg: TradingConfig): string {
  return [
    `T1 sells ${cfg.dynamic_targeting ? 'one lot' : `${cfg.target_1_exit_pct}%`}`,
    cfg.dynamic_targeting ? 'dynamic targets on' : null,
    cfg.pre_t1_trailing ? 'gains protected before T1' : null,
  ]
    .filter(Boolean)
    .join(' · ');
}

export function telegramLine(cfg: TradingConfig, flags: Flags | null): string {
  const valid = cfg.call_validity_minutes > 0 ? `Valid ${cfg.call_validity_minutes} min` : 'No time limit';
  return flags?.index_options_only ? `${valid} · index options only` : valid;
}

export function indicatorLine(cfg: TradingConfig): string {
  const window = cfg.indicator_strike_window === 0 ? 'all strikes' : `ATM ±${cfg.indicator_strike_window}`;
  return `${cfg.indicator_sizing ? 'On' : 'Off'} · ${cfg.indicator_interval_min}-minute checks · ${window}`;
}

/** "4 days", "3 h", "12 min". */
export function uptimeText(secs: number): string {
  if (secs >= 2 * 86_400) return `${Math.floor(secs / 86_400)} days`;
  if (secs >= 3600) return `${Math.floor(secs / 3600)} h`;
  return `${Math.max(1, Math.floor(secs / 60))} min`;
}

export function serverLine(system: SystemAnswer | null): string {
  if (!system) return '…';
  return system.uptime_secs != null ? `v${system.version} · up ${uptimeText(system.uptime_secs)}` : `v${system.version}`;
}

export function betaLine(flags: Flags | null): string {
  if (!flags) return '…';
  return `Index options only ${flags.index_options_only ? 'on' : 'off'} · chart stops ${flags.chart_stops ? 'on' : 'off'}`;
}
