import { useEffect, useMemo } from 'react';
import { AlignVerticalSpaceAround, Minus, MoveUpRight, Pencil, Redo2, SeparatorVertical, Slash, Trash2, Undo2, type LucideIcon } from 'lucide-react';
import { Popover } from '../ui/Popover';
import { isMac } from './chartKeymap';
import { DRAW_TOOLS, type DrawTool } from './drawings';

const ICONS: Record<DrawTool, LucideIcon> = {
  segment: Slash,
  horizontalStraightLine: Minus,
  rayLine: MoveUpRight,
  verticalStraightLine: SeparatorVertical,
  fibonacciLine: AlignVerticalSpaceAround,
};

/** The Draw button beside Layers, and its menu: the tools and Clear. */
export function DrawMenu({
  open,
  onToggle,
  onClose,
  onPick,
  onClear,
  onUndo,
  onRedo,
  canUndo = false,
  canRedo = false,
  disabled = false,
  compact = false,
}: {
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
  onPick: (tool: DrawTool) => void;
  onClear: () => void;
  onUndo?: () => void;
  onRedo?: () => void;
  canUndo?: boolean;
  canRedo?: boolean;
  /** "+ Stop" is being placed: the next tap is the stop's. */
  disabled?: boolean;
  /** Phone: no right-click to remove one drawing, only Clear. */
  compact?: boolean;
}) {
  const mac = useMemo(() => isMac(), []);
  return (
    <div className="relative shrink-0">
      <button
        type="button"
        aria-label="Draw"
        title={disabled ? 'Place the stop first' : 'Draw'}
        aria-expanded={open}
        disabled={disabled}
        onClick={onToggle}
        className="grid size-8 place-items-center rounded-6 text-fg-muted hover:bg-raised hover:text-fg disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-fg-muted"
      >
        <Pencil size={16} />
      </button>
      <Popover open={open && !disabled} onClose={onClose} align="right" backdrop={compact} scroll>
        <div className="px-2 pt-1 pb-1 type-caption text-fg-muted">Draw on this chart</div>
        {onUndo && (
          <button
            type="button"
            disabled={!canUndo}
            onClick={() => (onUndo(), onClose())}
            className="flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label text-fg hover:bg-raised disabled:opacity-40 disabled:hover:bg-transparent"
          >
            <Undo2 size={15} className="text-fg-muted" />
            <span>Undo</span>
            {!compact && <span className="ml-auto type-caption text-fg-muted">{mac ? '⌘Z' : 'Ctrl+Z'}</span>}
          </button>
        )}
        {onRedo && (
          <button
            type="button"
            disabled={!canRedo}
            onClick={() => (onRedo(), onClose())}
            className="flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label text-fg hover:bg-raised disabled:opacity-40 disabled:hover:bg-transparent"
          >
            <Redo2 size={15} className="text-fg-muted" />
            <span>Redo</span>
            {!compact && <span className="ml-auto type-caption text-fg-muted">{mac ? '⇧⌘Z' : 'Ctrl+Y'}</span>}
          </button>
        )}
        {(onUndo || onRedo) && <div className="my-1 h-px bg-line" />}
        {DRAW_TOOLS.map(({ tool, label }) => {
          const Icon = ICONS[tool];
          return (
            <button key={tool} type="button" onClick={() => (onPick(tool), onClose())} className="flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label text-fg hover:bg-raised">
              <Icon size={15} className="text-fg-muted" />
              {label}
            </button>
          );
        })}
        <div className="my-1 h-px bg-line" />
        <button type="button" onClick={() => (onClear(), onClose())} className="flex h-9 w-full items-center gap-2.5 rounded-6 px-2 type-label text-fg hover:bg-raised">
          <Trash2 size={15} className="text-fg-muted" />
          Clear drawings
        </button>
        <p className="px-2 pt-1 pb-1 type-caption text-fg-muted">{compact ? 'Kept for this instrument. Drag one to move it.' : 'Kept for this instrument. Drag one to move it; right-click it to remove it.'}</p>
      </Popover>
    </div>
  );
}

/** Over the chart while a tool is drawn: what to tap, and Cancel (or Escape). */
export function DrawBanner({ tool, onCancel }: { tool: DrawTool; onCancel: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onCancel();
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onCancel]);
  const ask = DRAW_TOOLS.find((t) => t.tool === tool)?.ask ?? 'Tap on the chart';
  return (
    <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center px-2">
      <div className="pointer-events-auto flex items-center gap-3 rounded-8 border border-line bg-panel px-3 py-1.5 shadow-lg">
        <span className="type-label text-fg">{ask}</span>
        <button type="button" onClick={onCancel} className="type-label text-fg-muted hover:text-fg">
          Cancel
        </button>
      </div>
    </div>
  );
}
