import type { ButtonHTMLAttributes, ReactNode } from 'react';

// Figma "Button". On jade, brick and marigold fills the text is dark.
export type ButtonKind = 'profit' | 'loss' | 'loss-outline' | 'neutral' | 'ghost' | 'alert';
export type ButtonSize = 'sm' | 'md' | 'lg';

const KIND: Record<ButtonKind, string> = {
  profit: 'bg-profit text-on-accent hover:brightness-110',
  loss: 'bg-loss text-on-accent hover:brightness-110',
  'loss-outline': 'border border-loss text-loss hover:bg-loss-subtle',
  neutral: 'bg-raised text-fg hover:bg-line',
  ghost: 'text-fg hover:bg-raised',
  alert: 'border border-loss text-loss hover:bg-loss-subtle',
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
  className = '',
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  kind?: ButtonKind;
  size?: ButtonSize;
  icon?: ReactNode;
}) {
  return (
    <button
      type="button"
      {...rest}
      className={`inline-flex shrink-0 select-none items-center justify-center whitespace-nowrap transition-[background-color,filter] disabled:cursor-not-allowed disabled:opacity-50 ${KIND[kind]} ${SIZE[size]} ${className}`}
    >
      {icon}
      {children}
    </button>
  );
}
