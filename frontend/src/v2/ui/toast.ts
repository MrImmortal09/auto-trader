import { createContext, useContext } from 'react';
import type { Tone } from '../lib/types';

export interface ToastSpec {
  title: string;
  body?: string;
  tone: Tone;
  /** The same key shown again within a minute is dropped, not repeated. */
  key?: string;
}

export const ToastContext = createContext<(t: ToastSpec) => void>(() => {});

/** Show a toast (see `ToastProvider` in Toasts.tsx). */
export function useToast() {
  return useContext(ToastContext);
}
