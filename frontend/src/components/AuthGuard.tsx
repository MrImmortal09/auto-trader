import { useState } from 'react';
import { isTokenValid } from '../lib/auth';
import { PasskeyScreen } from './PasskeyScreen';

export function AuthGuard({ children }: { children: React.ReactNode }) {
  const [authed, setAuthed] = useState(() => isTokenValid());

  if (!authed) {
    return <PasskeyScreen onSuccess={() => setAuthed(true)} />;
  }

  return <>{children}</>;
}
