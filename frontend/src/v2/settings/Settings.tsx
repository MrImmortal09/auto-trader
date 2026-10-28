import { useEffect, useState, type ReactNode } from 'react';
import { ChevronLeft, ChevronRight, CircleAlert } from 'lucide-react';
import { getFlags, getSystem } from '../lib/api';
import { usePoll } from '../lib/usePoll';
import type { HealthSet } from '../lib/types';
import { Button } from '../ui/Button';
import { IndicatorSizing, TelegramCalls } from './CallSections';
import { Connections } from './Connections';
import { Exits } from './Exits';
import { Lots } from './Lots';
import { Beta, ServerData, TradeNotes } from './OtherSections';
import { NavItem } from './parts';
import { betaLine, connectionsLine, exitsLine, indicatorLine, lotsLine, serverLine, telegramLine, tradingLine } from './summary';
import { Trading } from './Trading';
import { useSettings } from './useSettings';

type SectionId = 'connections' | 'trading' | 'lots' | 'exits' | 'telegram' | 'indicator' | 'trade' | 'server' | 'beta';

const SECTIONS: { id: SectionId; title: string }[] = [
  { id: 'connections', title: 'Connections' },
  { id: 'trading', title: 'Trading' },
  { id: 'lots', title: 'Lots per index' },
  { id: 'exits', title: 'Exits' },
  { id: 'telegram', title: 'Telegram calls' },
  { id: 'indicator', title: 'Indicator sizing' },
  { id: 'trade', title: 'Trade' },
  { id: 'server', title: 'Server and data' },
  { id: 'beta', title: 'Beta' },
];

const EXITS_NOTE = 'Saved as you change them. Open positions follow the new values too.';

/**
 * Settings: Connections, Trading, Lots per index, Exits and the rest, each
 * saved the moment it changes. Desktop lists the sections down one page
 * with a nav beside it; phone opens each on its own page.
 */
export function Settings({ serverBase, desktop, health, onKillSwitch, onModeChanged }: {
  serverBase: string;
  desktop: boolean;
  health: HealthSet | null;
  onKillSwitch: () => void;
  onModeChanged: () => void;
}) {
  const settings = useSettings(serverBase);
  const flags = usePoll(() => getFlags(serverBase), 30_000, serverBase);
  const system = usePoll(() => getSystem(serverBase), 60_000, serverBase);
  const [current, setCurrent] = useState<SectionId>('connections');
  const [opened, setOpened] = useState<SectionId | null>(null);
  const cfg = settings.cfg;
  const loaded = cfg != null;
  const indexOnly = flags.data ? flags.data.index_options_only : null;

  // The nav follows the section being read.
  useEffect(() => {
    if (!desktop || !loaded) return;
    const seen = new Map<string, boolean>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) seen.set(e.target.id, e.isIntersecting);
        const first = SECTIONS.find((s) => seen.get(s.id));
        if (first) setCurrent(first.id);
      },
      { rootMargin: '-15% 0px -65% 0px' },
    );
    for (const s of SECTIONS) {
      const el = document.getElementById(s.id);
      if (el) observer.observe(el);
    }
    return () => observer.disconnect();
  }, [desktop, loaded]);

  const body = (id: SectionId, withTitle: boolean): ReactNode => {
    const title = withTitle ? SECTIONS.find((s) => s.id === id)!.title : undefined;
    const sid = withTitle ? id : undefined;
    const large = !desktop;
    switch (id) {
      case 'connections':
        return <Connections id={sid} title={title} serverBase={serverBase} health={health} system={system.data} />;
      case 'trading':
        return <Trading id={sid} title={title} serverBase={serverBase} settings={settings} onKillSwitch={onKillSwitch} onModeChanged={onModeChanged} large={large} />;
      case 'lots':
        return <Lots id={sid} title={title} settings={settings} indexOnly={indexOnly} large={large} />;
      case 'exits':
        return (
          <div className="flex flex-col gap-3">
            <Exits id={sid} title={title} settings={settings} large={large} />
            <p className="type-caption text-fg-muted">{EXITS_NOTE}</p>
          </div>
        );
      case 'telegram':
        return <TelegramCalls id={sid} title={title} settings={settings} indexOnly={indexOnly} large={large} />;
      case 'indicator':
        return <IndicatorSizing id={sid} title={title} settings={settings} large={large} />;
      case 'trade':
        return <TradeNotes id={sid} title={title} flags={flags.data} />;
      case 'server':
        return <ServerData id={sid} title={title} serverBase={serverBase} settings={settings} system={system.data} onRestarted={() => void system.refresh()} large={large} />;
      case 'beta':
        return <Beta id={sid} title={title} serverBase={serverBase} flags={flags.data} onSaved={() => void flags.refresh()} />;
    }
  };

  if (!cfg) {
    return <p className="p-6 type-body text-fg-muted">{settings.error ?? 'Loading settings…'}</p>;
  }

  if (!desktop) {
    if (opened) {
      return (
        <div className="flex flex-col gap-3 p-4">
          <button type="button" onClick={() => setOpened(null)} className="flex items-center gap-1 self-start type-label text-fg-muted hover:text-fg">
            <ChevronLeft size={16} /> Settings
          </button>
          <h2 className="type-title text-fg">{SECTIONS.find((s) => s.id === opened)!.title}</h2>
          {body(opened, false)}
        </div>
      );
    }
    const lines: Record<SectionId, string> = {
      connections: connectionsLine(health),
      trading: tradingLine(cfg),
      lots: lotsLine(cfg),
      exits: exitsLine(cfg),
      telegram: telegramLine(cfg, flags.data),
      indicator: indicatorLine(cfg),
      trade: 'One-tap buy · any strike, any expiry · hold to exit all',
      server: serverLine(system.data),
      beta: betaLine(flags.data),
    };
    return (
      <div className="flex flex-col gap-4 p-4">
        <div className="rounded-12 bg-panel">
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setOpened(s.id)}
              className="flex min-h-[60px] w-full items-center gap-3 border-t border-line px-4 py-3 text-left first:border-t-0"
            >
              <span className="min-w-0 flex-1">
                <span className="block type-body-strong text-fg">{s.title}</span>
                <span className="mt-0.5 block type-caption text-fg-muted">{lines[s.id]}</span>
              </span>
              <ChevronRight size={16} className="shrink-0 text-fg-muted" />
            </button>
          ))}
        </div>
        {cfg.kill_switch_active ? (
          <p className="flex h-11 items-center justify-center gap-2 rounded-8 border border-loss type-button-lg text-loss">
            <CircleAlert size={18} /> Trading stopped
          </p>
        ) : (
          <Button size="lg" kind="alert" icon={<CircleAlert size={18} />} onClick={onKillSwitch} className="w-full">
            Kill switch
          </Button>
        )}
      </div>
    );
  }

  return (
    <div className="flex min-h-full">
      <nav aria-label="Settings sections" className="w-[220px] shrink-0 border-r border-line">
        <div className="sticky top-0 flex flex-col gap-1 p-3 pt-5">
          {SECTIONS.map((s) => (
            <NavItem
              key={s.id}
              selected={current === s.id}
              onClick={() => {
                setCurrent(s.id);
                document.getElementById(s.id)?.scrollIntoView({ behavior: 'smooth', block: 'start' });
              }}
            >
              {s.title}
            </NavItem>
          ))}
        </div>
      </nav>
      <div className="min-w-0 flex-1 px-10 pt-6 pb-24">
        <div className="flex max-w-[760px] flex-col gap-8">{SECTIONS.map((s) => <div key={s.id}>{body(s.id, true)}</div>)}</div>
      </div>
    </div>
  );
}
