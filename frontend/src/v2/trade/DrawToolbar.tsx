import { useMemo, useRef, useState, type ReactNode } from 'react';
import { Eraser, Eye, EyeOff, Lock, LockOpen, Magnet, MousePointer2, Redo2, Trash2, Undo2, type LucideIcon } from 'lucide-react';
import { isMac } from './chartKeymap';
import { DRAW_ICONS } from './drawIcons';
import { DRAW_TOOLS, type DrawPrefs, type DrawTool } from './drawings';

/** Tools that share a button: it shows the one last picked; its corner
 * opens the rest. */
const GROUPS: { id: string; label: string; tools: DrawTool[] }[] = [
  { id: 'lines', label: 'Lines', tools: ['segment', 'rayLine', 'straightLine', 'horizontalStraightLine', 'horizontalRayLine', 'horizontalSegment', 'verticalStraightLine', 'priceLine'] },
  { id: 'fib', label: 'Fibonacci and channels', tools: ['fibonacciLine', 'parallelStraightLine', 'priceChannelLine'] },
];
/** Tools with a button each, after the groups. */
const SINGLES: DrawTool[] = ['v2Path', 'v2Rect', 'brush', 'v2Text', 'simpleAnnotation'];

const LABEL = new Map(DRAW_TOOLS.map((t) => [t.tool, t.label]));
const BTN = 'relative grid size-8 shrink-0 place-items-center rounded-6 transition-colors duration-120 disabled:opacity-40 disabled:hover:bg-transparent';
const look = (on: boolean) => (on ? 'bg-accent-subtle text-accent' : 'text-fg-muted hover:bg-raised hover:text-fg');

function Sep() {
  return <span aria-hidden className="my-1 h-px w-5 shrink-0 bg-line" />;
}

function Button({ icon: Icon, label, on = false, pressed, disabled, onClick, children }: { icon: LucideIcon; label: string; on?: boolean; pressed?: boolean; disabled?: boolean; onClick: () => void; children?: ReactNode }) {
  return (
    <button type="button" aria-label={label} title={label} aria-pressed={pressed} disabled={disabled} onClick={onClick} className={`${BTN} ${look(on)}`}>
      <Icon size={16} />
      {children}
    </button>
  );
}

/**
 * The desktop KLine chart's drawing toolbar, down its left edge like Vela's:
 * the cursor, the tools, the ruler and eraser, the magnet, lock and hide
 * switches (the same on every KLine chart), then undo, redo and Clear. It
 * scrolls when the chart is too short for all of it.
 */
export function DrawToolbar({ active, erasing, prefs, onPick, onCursor, onErase, onPrefs, onUndo, onRedo, onClear, disabled }: {
  /** The tool being drawn, if any. */
  active: DrawTool | null;
  erasing: boolean;
  prefs: DrawPrefs;
  onPick: (tool: DrawTool) => void;
  onCursor: () => void;
  onErase: () => void;
  onPrefs: (change: Partial<DrawPrefs>) => void;
  onUndo: () => void;
  onRedo: () => void;
  onClear: () => void;
  /** "+ Stop" is being placed: the next tap is the stop's. */
  disabled: boolean;
}) {
  const mac = useMemo(() => isMac(), []);
  // Each group's tool on its button: the last one picked from it.
  const [shown, setShown] = useState<Record<string, DrawTool>>(() => Object.fromEntries(GROUPS.map((g) => [g.id, g.tools[0]])));
  const [flyout, setFlyout] = useState<{ id: string; x: number; y: number } | null>(null);
  const corner = useRef(new Map<string, HTMLButtonElement>());

  const pick = (tool: DrawTool) => {
    setFlyout(null);
    if (tool === active) return onCursor();
    const group = GROUPS.find((g) => g.tools.includes(tool));
    if (group) setShown((s) => ({ ...s, [group.id]: tool }));
    onPick(tool);
  };
  const openFlyout = (id: string) => {
    if (flyout?.id === id) return setFlyout(null);
    const r = corner.current.get(id)?.parentElement?.getBoundingClientRect();
    // Fixed, so the toolbar's own scrolling never clips it.
    if (r) setFlyout({ id, x: r.right + 6, y: r.top });
  };
  const open = GROUPS.find((g) => g.id === flyout?.id);

  return (
    <div role="toolbar" aria-label="Drawing tools" aria-orientation="vertical" className="flex w-10 shrink-0 flex-col items-center gap-0.5 overflow-y-auto border-r border-line py-1.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
      <Button icon={MousePointer2} label="Cursor" on={active == null && !erasing} disabled={disabled} onClick={onCursor} />
      <Sep />
      {GROUPS.map((g) => {
        const tool = g.tools.includes(active as DrawTool) ? (active as DrawTool) : shown[g.id];
        return (
          <div key={g.id} className="relative shrink-0">
            <Button icon={DRAW_ICONS[tool]} label={LABEL.get(tool) ?? g.label} on={g.tools.includes(active as DrawTool)} disabled={disabled} onClick={() => pick(tool)} />
            <button
              ref={(el) => {
                if (el) corner.current.set(g.id, el);
                else corner.current.delete(g.id);
              }}
              type="button"
              aria-label={`More: ${g.label}`}
              title={g.label}
              aria-expanded={flyout?.id === g.id}
              disabled={disabled}
              onClick={() => openFlyout(g.id)}
              className="absolute right-0 bottom-0 grid size-3 place-items-end rounded-2 text-fg-subtle hover:text-fg disabled:opacity-40"
            >
              <span aria-hidden className="mr-0.5 mb-0.5 size-0 border-b-[4px] border-l-[4px] border-b-current border-l-transparent" />
            </button>
          </div>
        );
      })}
      {SINGLES.map((tool) => (
        <Button key={tool} icon={DRAW_ICONS[tool]} label={LABEL.get(tool) ?? tool} on={active === tool} disabled={disabled} onClick={() => pick(tool)} />
      ))}
      <Sep />
      <Button icon={DRAW_ICONS.v2Ruler} label="Measure" on={active === 'v2Ruler'} disabled={disabled} onClick={() => pick('v2Ruler')} />
      <Button icon={Eraser} label="Eraser: tap a drawing to remove it" on={erasing} pressed={erasing} disabled={disabled} onClick={onErase} />
      <Button icon={Magnet} label={prefs.magnet ? 'Magnet on: points snap to the bars' : 'Magnet: snap points to the bars'} on={prefs.magnet} pressed={prefs.magnet} onClick={() => onPrefs({ magnet: !prefs.magnet })} />
      <Button icon={prefs.locked ? Lock : LockOpen} label={prefs.locked ? 'Unlock drawings' : 'Lock drawings'} on={prefs.locked} pressed={prefs.locked} onClick={() => onPrefs({ locked: !prefs.locked })} />
      <Button icon={prefs.hidden ? EyeOff : Eye} label={prefs.hidden ? 'Show drawings' : 'Hide drawings'} on={prefs.hidden} pressed={prefs.hidden} onClick={() => onPrefs({ hidden: !prefs.hidden })} />
      <Sep />
      <Button icon={Undo2} label={`Undo (${mac ? '⌘Z' : 'Ctrl+Z'})`} disabled={disabled} onClick={onUndo} />
      <Button icon={Redo2} label={`Redo (${mac ? '⇧⌘Z' : 'Ctrl+Y'})`} disabled={disabled} onClick={onRedo} />
      <Button icon={Trash2} label="Clear drawings" disabled={disabled} onClick={onClear} />
      {open && flyout && (
        <>
          <div aria-hidden className="fixed inset-0 z-30" onClick={() => setFlyout(null)} />
          <div role="menu" aria-label={open.label} style={{ left: flyout.x, top: flyout.y }} className="fixed z-40 min-w-[200px] animate-fade-in rounded-12 border border-line bg-panel p-1.5 shadow-md">
            <div className="px-2 pt-1 pb-1 type-caption text-fg-muted">{open.label}</div>
            {open.tools.map((tool) => {
              const Icon = DRAW_ICONS[tool];
              const on = tool === active;
              return (
                <button key={tool} type="button" role="menuitem" onClick={() => pick(tool)} className={`flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label ${on ? 'bg-accent-subtle text-accent' : 'text-fg hover:bg-raised'}`}>
                  <Icon size={15} className={on ? 'text-accent' : 'text-fg-muted'} />
                  {LABEL.get(tool)}
                </button>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}
