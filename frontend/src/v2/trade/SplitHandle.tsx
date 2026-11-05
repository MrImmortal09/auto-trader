import { SPLIT_HANDLE, type PhoneSplit } from './phoneSplit';

/** The bar between the chart and the ladder: drag it to share the height. */
export function SplitHandle({ height, dragging, handle }: Pick<PhoneSplit, 'height' | 'dragging' | 'handle'>) {
  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      aria-label="Drag to resize the chart"
      aria-valuenow={height}
      tabIndex={0}
      title="Drag to resize the chart"
      {...handle}
      style={{ height: SPLIT_HANDLE }}
      className="group flex shrink-0 cursor-row-resize touch-none select-none items-center justify-center"
    >
      <span className={`h-1 w-10 rounded-full transition-colors ${dragging ? 'bg-fg-muted' : 'bg-line-strong group-hover:bg-fg-muted'}`} />
    </div>
  );
}
