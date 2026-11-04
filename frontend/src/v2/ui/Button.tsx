import type { ButtonHTMLAttributes, ReactNode } from 'react';

// Figma "Button". A filled kind carries white text and darkens to its
// *-strong token on hover (never brightens); LIVE is marigold with ink text
// and its ink edge. Blue is what you press that is not an order; green is
// Buy and Add only; red is Exit, Sell and the kill switch.
export type ButtonKind = 'accent' | 'profit' | 'loss' | 'loss-outline' | 'neutral' | 'ghost' | 'quiet' | 'alert' | 'live';
export type ButtonSize = 'sm' | 'md' | 'lg';

const KIND: Record<ButtonKind, string> = {
  // Send code, Sign in, Add, Save, Reconnect, Add funds.
  accent: 'bg-accent text-on-accent hover:bg-accent-strong active:brightness-90',
  // Buy and Add only.
  profit: 'bg-profit text-on-accent hover:bg-profit-strong active:brightness-90',
  // Exit, Sell, Remove, the kill-switch hold.
  loss: 'bg-loss text-on-accent hover:bg-loss-strong active:brightness-90',
  'loss-outline': 'border border-loss text-loss hover:bg-loss-subtle',
  neutral: 'bg-raised text-fg hover:bg-raised-strong',
  ghost: 'text-fg hover:bg-raised',
  quiet: 'text-fg-muted hover:bg-raised hover:text-fg',
  alert: 'border border-loss text-loss hover:bg-loss-subtle',
  // Real money: marigold, only for going LIVE.
  live: 'bg-live text-fg border border-live-ink hover:bg-live-ink hover:text-on-accent active:brightness-90',
};

const SIZE: Record<ButtonSize, string> = {
  // Desktop rows and bars.
  sm: 'h-7 px-2.5 rounded-6 type-button gap-1.5',
  md: 'h-8 px-3 rounded-6 type-button gap-1.5',
  // Phone: a 44 pt touch target.
  lg: 'h-11 px-4 rounded-8 type-button-lg gap-2',
};

export function Button({
  kind = 'neutral',
  size = 'sm',
  icon,
  pointerOnly = false,
  onClick,
  className = '',
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  kind?: ButtonKind;
  size?: ButtonSize;
  icon?: ReactNode;
  /** For a button that sends an order: no keyboard key ever sends one, so a
   * click from Enter or Space (`detail` 0) is ignored. */
  pointerOnly?: boolean;
}) {
  return (
    <button
      type="button"
      {...rest}
      onClick={
        pointerOnly && onClick
          ? (e) => {
              if (e.detail !== 0) onClick(e);
            }
          : onClick
      }
      className={`inline-flex shrink-0 select-none items-center justify-center whitespace-nowrap transition-[background-color,color] duration-120 disabled:cursor-not-allowed disabled:opacity-50 ${KIND[kind]} ${SIZE[size]} ${className}`}
    >
      {icon}
      {children}
    </button>
  );
}
