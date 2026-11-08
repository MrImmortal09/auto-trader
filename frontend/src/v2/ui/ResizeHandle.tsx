import { useRef, useState, type KeyboardEvent, type PointerEvent } from 'react';

/** A key press moves the edge this far; with Shift, `BIG_STEP`. */
const STEP = 16;
const BIG_STEP = 64;

const clamp = (v: number, min: number, max: number) => Math.round(Math.max(min, Math.min(max, v)));

/**
 * The draggable edge between two side-by-side panes. `value` is the width of
 * the pane it sizes; dragging (or the arrow keys, Shift for bigger steps)
 * moves it within `min`..`max`, double-click or Home puts `onReset`'s default
 * back. `grows` says which way dragging makes that pane wider: `right` when
 * the pane is left of the edge, `left` when it is right of it.
 *
 * `onChange` follows the pointer; `onCommit` gets the value to keep once the
 * drag ends, so a drag saves once, not on every move.
 *
 * It is the 1 px line between two flat panes, takes 1 px of the row, and is
 * grabbed 4 px either side of it.
 */
export function ResizeHandle({ label, value, min, max, grows, onChange, onCommit, onReset, className = '' }: {
  label: string;
  value: number;
  min: number;
  max: number;
  grows: 'left' | 'right';
  onChange: (v: number) => void;
  onCommit: (v: number) => void;
  onReset: () => void;
  className?: string;
}) {
  const sign = grows === 'right' ? 1 : -1;
  const drag = useRef<{ x: number; v: number; last: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const onDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    drag.current = { x: e.clientX, v: value, last: value };
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragging(true);
  };
  const onMove = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    d.last = clamp(d.v + sign * (e.clientX - d.x), min, max);
    onChange(d.last);
  };
  const onUp = () => {
    const d = drag.current;
    drag.current = null;
    setDragging(false);
    if (d && d.last !== d.v) onCommit(d.last);
  };
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? BIG_STEP : STEP;
    const by = e.key === 'ArrowRight' ? step : e.key === 'ArrowLeft' ? -step : 0;
    if (by !== 0) {
      e.preventDefault();
      onCommit(clamp(value + sign * by, min, max));
    } else if (e.key === 'Home') {
      e.preventDefault();
      onReset();
    }
  };

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={Math.round(value)}
      aria-valuemin={Math.round(min)}
      aria-valuemax={Math.round(max)}
      tabIndex={0}
      title={`${label}: drag, or double-click for the default size`}
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
      onDoubleClick={onReset}
      onKeyDown={onKey}
      className={`group relative z-10 w-px shrink-0 cursor-col-resize touch-none select-none bg-line outline-none ${className}`}
    >
      {/* The grip, 4 px either side of the line, and what shows on hover or drag. */}
      <span aria-hidden className="absolute inset-y-0 -right-1 -left-1" />
      <span
        aria-hidden
        className={`pointer-events-none absolute inset-y-0 left-1/2 w-[3px] -translate-x-1/2 transition-colors duration-120 ${dragging ? 'bg-accent' : 'bg-transparent group-hover:bg-line-strong group-focus-visible:bg-accent'}`}
      />
    </div>
  );
}
