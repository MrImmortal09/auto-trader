import { useEffect, useRef, type ReactNode } from 'react';

/**
 * The body of a menu of actions (`role="menu"`), for a Popover: focus lands
 * on its first enabled item when it opens, ↑/↓ move between the enabled
 * items (wrapping), Home and End jump to the ends, and when it closes focus
 * goes back to where it was — the button that opened it — unless the close
 * put it somewhere else (a dialog the item opened, a button clicked
 * outside). Tab still leaves the menu as it would any group of buttons; the
 * Popover around it handles Escape. Items are `<button role="menuitem">`
 * children; a disabled one is skipped.
 */
export function Menu({ label, className = '', children }: {
  /** What a screen reader calls the menu. */
  label: string;
  className?: string;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  // What had focus as the menu opened (its button), kept across a re-run of
  // the effect (StrictMode), when the first item has it already.
  const opener = useRef<HTMLElement | null>(null);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const active = document.activeElement;
    if (active instanceof HTMLElement && !el.contains(active)) opener.current = active;
    itemsOf(el)[0]?.focus();
    return () => {
      // The menu's node is gone by now, so focus has fallen to the body —
      // unless the press that closed it landed on something focusable.
      const was = opener.current;
      if (was?.isConnected && (!document.activeElement || document.activeElement === document.body)) was.focus();
    };
  }, []);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    const items = itemsOf(box.current);
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    let next: number;
    if (e.key === 'ArrowDown') next = at < 0 ? 0 : (at + 1) % items.length;
    else if (e.key === 'ArrowUp') next = at < 0 ? items.length - 1 : (at - 1 + items.length) % items.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = items.length - 1;
    else return;
    e.preventDefault();
    items[next].focus();
  }

  return (
    <div ref={box} role="menu" aria-label={label} onKeyDown={onKeyDown} className={`flex flex-col ${className}`}>
      {children}
    </div>
  );
}

/** The menu's items that can take focus, in order. */
function itemsOf(el: HTMLElement | null): HTMLButtonElement[] {
  return el ? [...el.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')] : [];
}
