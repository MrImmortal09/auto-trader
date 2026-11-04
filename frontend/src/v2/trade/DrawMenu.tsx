import { useEffect } from 'react';
import { AlignVerticalSpaceAround, Minus, MoveUpRight, Pencil, SeparatorVertical, Slash, Trash2, type LucideIcon } from 'lucide-react';
import { Popover } from '../ui/Popover';
import { DRAW_TOOLS, type DrawTool } from './drawings';

const ICONS: Record<DrawTool, LucideIcon> = {
  segment: Slash,
  horizontalStraightLine: Minus,
  rayLine: MoveUpRight,
  verticalStraightLine: SeparatorVertical,
  fibonacciLine: AlignVerticalSpaceAround,
};

const ROW = 'flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label';

/** The Draw button beside Layers, and its menu: the tools and Clear. While a
 * tool is being drawn the pencil is lit and its row is the selected one. */
export function DrawMenu({ open, onToggle, onClose, onPick, onClear, active = null, disabled = false, compact = false }: {
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
  onPick: (tool: DrawTool) => void;
  onClear: () => void;
  /** The tool being drawn, if any. */
  active?: DrawTool | null;
  /** "+ Stop" is being placed: the next tap is the stop's. */
  disabled?: boolean;
  /** Phone: no right-click to remove one drawing, only Clear. */
  compact?: boolean;
}) {
  return (
    <div className="relative">
      <button
        type="button"
        aria-label="Draw"
        title={disabled ? 'Place the stop first' : 'Draw'}
        aria-expanded={open}
        disabled={disabled}
        onClick={onToggle}
        className={`grid size-8 place-items-center rounded-6 transition-colors duration-120 hover:bg-raised disabled:opacity-50 disabled:hover:bg-transparent ${
          active ? 'text-accent' : 'text-fg-muted hover:text-fg disabled:hover:text-fg-muted'
        }`}
      >
        <Pencil size={16} />
      </button>
      <Popover open={open && !disabled} onClose={onClose} align="right">
        <div className="px-2 pt-1 pb-1 type-caption text-fg-muted">Draw on this chart</div>
        {DRAW_TOOLS.map(({ tool, label }) => {
          const Icon = ICONS[tool];
          const on = tool === active;
          return (
            <button key={tool} type="button" onClick={() => (onPick(tool), onClose())} className={`${ROW} ${on ? 'bg-accent-subtle text-accent' : 'text-fg hover:bg-raised'}`}>
              <Icon size={15} className={on ? 'text-accent' : 'text-fg-muted'} />
              {label}
            </button>
          );
        })}
        <div className="my-1 h-px bg-line" />
        <button type="button" onClick={() => (onClear(), onClose())} className={`${ROW} text-fg hover:bg-raised`}>
          <Trash2 size={15} className="text-fg-muted" />
          Clear drawings
        </button>
        <p className="px-2 pt-1 pb-1 type-caption text-fg-subtle">{compact ? 'Kept for this instrument. Drag one to move it.' : 'Kept for this instrument. Drag one to move it; right-click it to remove it.'}</p>
      </Popover>
    </div>
  );
}

/** Over the chart while a tool is drawn: what to tap, and Cancel (or Escape).
 * The same floating pill as "Tap where the stop goes". */
export function DrawBanner({ tool, onCancel }: { tool: DrawTool; onCancel: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onCancel();
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onCancel]);
  const ask = DRAW_TOOLS.find((t) => t.tool === tool)?.ask ?? 'Tap on the chart';
  return (
    <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center px-2">
      <div className="pointer-events-auto flex items-center gap-3 rounded-8 border border-line bg-panel px-3 py-1.5 shadow-md">
        <span className="type-label text-fg">{ask}</span>
        <button type="button" onClick={onCancel} className="type-label text-fg-muted hover:text-fg">
          Cancel
        </button>
      </div>
    </div>
  );
}
