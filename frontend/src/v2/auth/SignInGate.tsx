import { useState, type ReactNode } from 'react';
import { useServerBase } from '../lib/hooks';
import { ServerDialog } from '../settings/dialogs';
import { useIdentity } from './identity';
import { SignInForm } from './SignInSheet';

/**
 * Nothing opens without a sign-in: until there's one, the whole page is the
 * sign-in, and nothing else is fetched. A sign-out, a refused token or a
 * disabled user comes back here.
 */
export function SignInGate({ children }: { children: ReactNode }) {
  const { role } = useIdentity();
  return role === 'anon' ? <SignInPage /> : children;
}

function SignInPage() {
  const serverBase = useServerBase();
  const { signInReason } = useIdentity();
  const [serverOpen, setServerOpen] = useState(false);
  const server = (serverBase || window.location.origin).replace(/^https?:\/\//, '');
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center bg-canvas px-4 py-10 text-fg">
      <div className="flex w-full max-w-[400px] flex-col gap-6">
        <div className="flex items-center gap-3">
          <div aria-hidden className="grid size-10 place-items-center rounded-8 bg-raised type-label font-semibold text-fg">
            AT
          </div>
          <div>
            <h1 className="type-title text-fg">Auto Trader</h1>
            <p className="type-caption text-fg-muted">For invited numbers only.</p>
          </div>
        </div>
        <div className="rounded-16 border border-line bg-panel">
          <SignInForm reason={signInReason} />
        </div>
        <p className="type-caption text-fg-muted">
          Server {server} ·{' '}
          <button type="button" className="text-fg underline-offset-2 hover:underline" onClick={() => setServerOpen(true)}>
            Change
          </button>
        </p>
      </div>
      {serverOpen && <ServerDialog current={serverBase} onClose={() => setServerOpen(false)} />}
    </div>
  );
}
