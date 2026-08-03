import { useEffect, useState } from 'react';

export function QtyInput({ initialQty, id, defaultQty, onUpdate }: {
  initialQty: number | null;
  id: string;
  defaultQty?: string;
  onUpdate: (id: string, q: number | null) => void;
}) {
  const [val, setVal] = useState(initialQty === null ? '' : String(initialQty));

  useEffect(() => {
    setVal(initialQty === null ? '' : String(initialQty));
  }, [initialQty]);

  return (
    <input
      type="number"
      value={val}
      placeholder={defaultQty ? `Auto (${defaultQty})` : "Auto"}
      onChange={e => setVal(e.target.value)}
      onBlur={() => {
        const parsed = parseInt(val, 10);
        const finalVal = isNaN(parsed) ? null : parsed;
        if (finalVal !== initialQty) {
          onUpdate(id, finalVal);
        }
      }}
      className="w-20 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code"
    />
  );
}
