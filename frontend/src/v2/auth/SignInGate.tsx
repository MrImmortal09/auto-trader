import { useState, type ReactNode } from 'react';
import { useServerBase } from '../lib/hooks';
import { ServerDialog } from '../settings/dialogs';
import { Mark } from '../shell/Mark';
import { useIdentity } from './identity';
import { SignInForm } from './SignInSheet';
import { ViewerPage } from './ViewerPage';
import { isViewPath, VIEW_PATH } from './viewPath';

/**
 * Nothing opens without a sign-in: until there's one, the whole page is the
 * sign-in, and nothing else is fetched. A sign-out, a refused token or a
 * disabled user comes back here. The one page that opens without it is the
 * view-only page (`/view`), for anyone, signed in or not.
 */
export function SignInGate({ children }: { children: ReactNode }) {
  const { role } = useIdentity();
  if (isViewPath()) return <ViewerPage />;
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
          <Mark />
          <div>
            <h1 className="type-title text-fg">Auto Trader</h1>
            <p className="type-caption text-fg-subtle">For invited numbers only.</p>
          </div>
        </div>
        <div className="rounded-16 border border-line bg-panel shadow-lg">
          <SignInForm reason={signInReason} />
        </div>
        <a
          href={VIEW_PATH}
          className="flex items-center justify-between gap-3 rounded-12 border border-line bg-panel px-4 py-3 transition-colors duration-120 hover:border-accent"
        >
          <span>
            <span className="block type-body-strong text-fg">View someone's trades</span>
            <span className="block type-caption text-fg-subtle">View only, by mobile number. No code needed.</span>
          </span>
          <span className="type-body text-accent">→</span>
        </a>
        <p className="type-caption text-fg-subtle">
          Server {server} ·{' '}
          <button type="button" className="text-accent underline-offset-2 transition-colors duration-120 hover:text-accent-strong hover:underline" onClick={() => setServerOpen(true)}>
            Change
          </button>
        </p>
      </div>
      {serverOpen && <ServerDialog current={serverBase} onClose={() => setServerOpen(false)} />}
    </div>
  );
}
