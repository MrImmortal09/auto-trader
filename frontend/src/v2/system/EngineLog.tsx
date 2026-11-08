import { useState } from 'react';
import { Search } from 'lucide-react';
import { shortDate, todayIst } from '../lib/format';
import { Segment } from '../ui/Segment';
import { Toggle } from '../ui/Toggle';
import { KIND_LABEL, type LogKind, type LogLine } from './logLines';

type Filter = 'all' | Exclude<LogKind, 'info'>;

const FILTERS: { value: Filter; label: string }[] = [
  { value: 'all', label: 'All' },
  { value: 'order', label: 'Orders' },
  { value: 'signal', label: 'Signals' },
  { value: 'error', label: 'Errors' },
];

// Grey for the everyday kinds (a faint edge, a raised fill, a strong edge);
// red only on an error.
const LOOK: Record<LogKind, string> = {
  info: 'border border-line-strong/50 text-fg-muted',
  order: 'bg-raised text-fg',
  signal: 'border border-line-strong text-fg',
  error: 'bg-loss-subtle text-loss',
};

/** Figma "Log row" kind chip. */
function KindChip({ kind }: { kind: LogKind }) {
  return <span className={`inline-flex h-5 w-14 shrink-0 items-center justify-center rounded-4 type-caption ${LOOK[kind]}`}>{KIND_LABEL[kind]}</span>;
}

/** `11:46:01` today, `21 Oct 15:39` before. */
function when(at: string, today: string): string {
  return at.startsWith(today) ? at.slice(11, 19) : `${shortDate(at)} ${at.slice(11, 16)}`;
}

/** Figma "Log row", in the mono face so the times line up: an error's text
 * is red, everything else ink. */
function Row({ line, today, compact }: { line: LogLine; today: string; compact: boolean }) {
  const ink = line.kind === 'error' ? 'text-loss' : 'text-fg';
  if (compact) {
    return (
      <li className="border-b border-line py-2.5 last:border-0">
        <div className="mb-1 flex items-center gap-2">
          <KindChip kind={line.kind} />
          <span className="type-mono text-fg-subtle">{when(line.at, today)}</span>
        </div>
        <span className={`block min-w-0 break-words type-mono ${ink}`}>{line.text}</span>
      </li>
    );
  }
  // 96 px holds "21 Oct 15:39"; the 2 px top padding centres a line on the chip.
  return (
    <li className="grid grid-cols-[96px_56px_minmax(0,1fr)] items-start gap-4 py-2">
      <span className="pt-0.5 type-mono text-fg-subtle">{when(line.at, today)}</span>
      <KindChip kind={line.kind} />
      <span className={`min-w-0 break-words pt-0.5 type-mono ${ink}`}>{line.text}</span>
    </li>
  );
}

/** The newest few lines, for the phone's System card. */
export function LogPreview({ lines, count }: { lines: LogLine[]; count: number }) {
  const today = todayIst();
  const shown = lines.slice(0, count);
  if (shown.length === 0) return <p className="py-4 type-body text-fg-subtle">Waiting for engine events…</p>;
  return (
    <ul>
      {shown.map((l) => (
        <Row key={l.id} line={l} today={today} compact />
      ))}
    </ul>
  );
}

/**
 * The engine log: newest first, filtered to orders, signals or errors and
 * searched. With Follow off it holds still, and says how much has arrived.
 */
export function EngineLog({ lines, connected, compact, maxHeight }: {
  lines: LogLine[];
  connected: boolean | null;
  compact: boolean;
  /** Scroll inside this height instead of growing the page. */
  maxHeight?: number;
}) {
  const [filter, setFilter] = useState<Filter>('all');
  const [query, setQuery] = useState('');
  // What was showing when Follow went off.
  const [held, setHeld] = useState<LogLine[] | null>(null);
  const today = todayIst();
  const source = held ?? lines;
  const q = query.trim().toLowerCase();
  const shown = source.filter((l) => (filter === 'all' || l.kind === filter) && (!q || l.text.toLowerCase().includes(q)));
  const newer = held == null ? 0 : held.length === 0 ? lines.length : Math.max(0, lines.findIndex((l) => l.id === held[0].id));

  const follow = (
    <label className="flex items-center gap-2 type-label text-fg-muted">
      Follow
      <Toggle label="Follow new lines" checked={held == null} onChange={(on) => setHeld(on ? null : lines)} />
    </label>
  );
  // An inset well with a hairline; its edge turns accent while it has focus, and it carries the focus ring for its input.
  const search = (
    <label className={`flex h-8 items-center gap-2 rounded-6 border border-line bg-inset px-2.5 transition-colors duration-120 focus-within:border-accent has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-accent has-[:focus-visible]:outline-offset-2 ${compact ? 'w-full' : 'w-[220px]'}`}>
      <Search size={14} className="shrink-0 text-fg-muted" />
      <input
        type="search"
        aria-label="Search the log"
        placeholder="Search the log"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        className="min-w-0 flex-1 bg-transparent type-body text-fg outline-none placeholder:text-fg-subtle"
      />
    </label>
  );

  return (
    <div className="flex min-w-0 flex-col">
      {compact ? (
        <div className="flex flex-col gap-2">
          <Segment label="Show" stretch value={filter} onChange={setFilter} options={FILTERS} />
          <div className="flex items-center gap-3">
            {search}
            {follow}
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-3">
          <h3 className="type-title text-fg">Engine log</h3>
          <Segment label="Show" value={filter} onChange={setFilter} options={FILTERS} />
          {connected === false && <span className="type-caption text-loss">Reconnecting…</span>}
          <span className="flex-1" />
          {search}
          {follow}
        </div>
      )}
      {newer > 0 && (
        <button
          type="button"
          onClick={() => setHeld(null)}
          className="mt-2 self-start rounded-6 bg-raised px-2.5 py-1 type-label text-fg transition-colors duration-120 hover:bg-raised-strong"
        >
          {newer} new {newer === 1 ? 'line' : 'lines'} · show
        </button>
      )}
      {shown.length === 0 ? (
        <p className="py-6 type-body text-fg-subtle">{source.length === 0 ? 'Waiting for engine events…' : 'Nothing matches.'}</p>
      ) : (
        <ul className="mt-2 overflow-y-auto" style={maxHeight ? { maxHeight } : undefined}>
          {shown.map((l) => (
            <Row key={l.id} line={l} today={today} compact={compact} />
          ))}
        </ul>
      )}
    </div>
  );
}
