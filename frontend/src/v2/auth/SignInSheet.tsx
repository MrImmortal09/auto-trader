import { useEffect, useState } from 'react';
import { useAuth } from '../../context/AuthContext';
import { setUserToken } from '../../lib/session';
import { requestOtp, verifyOtp } from '../lib/api';
import { useServerBase } from '../lib/hooks';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { useToast } from '../ui/toast';
import { useIdentity } from './identity';
import { displayName, formatPhone, normalizePhone } from './phone';

/**
 * Sign in with a phone number: a code arrives on WhatsApp, six digits
 * finish it. Only numbers the admin has added get a code. The owner's
 * passkey is a step away, for the owner on a fresh browser.
 */
export function SignInSheet() {
  const { signInOpen, closeSignIn, signInReason } = useIdentity();
  // Mounted fresh each time, so it never opens on a stale step or error.
  return signInOpen ? <SignInForm reason={signInReason} onClose={closeSignIn} /> : null;
}

const INPUT = 'h-12 w-full rounded-8 border border-line bg-inset px-3 text-fg outline-none focus:border-line-strong disabled:opacity-50';

function mmss(secs: number): string {
  return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, '0')}`;
}

function SignInForm({ reason, onClose }: { reason: string | null; onClose: () => void }) {
  const serverBase = useServerBase();
  const { openUnlockModal } = useAuth();
  const toast = useToast();
  const [step, setStep] = useState<'phone' | 'code'>('phone');
  const [typed, setTyped] = useState('');
  const [phone, setPhone] = useState<string | null>(null);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // "Send again" waits this long after a send; a 429 locks everything longer.
  const [resendAt, setResendAt] = useState<number | null>(null);
  const [lockedUntil, setLockedUntil] = useState<number | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (resendAt == null && lockedUntil == null) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [resendAt, lockedUntil]);

  const lockedFor = lockedUntil == null ? 0 : Math.max(0, Math.ceil((lockedUntil - now) / 1000));
  const locked = lockedFor > 0;
  const resendIn = resendAt == null ? 0 : Math.max(0, Math.ceil((resendAt - now) / 1000));

  async function sendCode(to: string) {
    setBusy(true);
    setError(null);
    const r = await requestOtp(serverBase, to);
    setBusy(false);
    if (r.ok) {
      setPhone(to);
      setStep('code');
      setCode('');
      setNow(Date.now());
      setResendAt(Date.now() + (r.retryAfter ?? 30) * 1000);
      return;
    }
    if (r.lockedForSecs) {
      setNow(Date.now());
      setLockedUntil(Date.now() + r.lockedForSecs * 1000);
    }
    setError(r.error ?? "Couldn't send the code.");
  }

  function submitPhone() {
    const normalized = normalizePhone(typed);
    if (!normalized) {
      setError('Enter a 10-digit Indian mobile number.');
      return;
    }
    void sendCode(normalized);
  }

  async function submitCode(value: string) {
    if (!phone || value.length !== 6 || busy || locked) return;
    setBusy(true);
    setError(null);
    const answer = await verifyOtp(serverBase, phone, value);
    setBusy(false);
    if ('token' in answer) {
      setUserToken(answer.token);
      toast({ tone: 'profit', title: `Signed in as ${displayName(answer.user)}`, body: answer.user.role === 'admin' ? 'Admin: you see everything; real-money actions still ask for the passkey.' : 'Paper trading on your own portfolio.' });
      onClose();
      return;
    }
    setCode('');
    if (answer.lockedForSecs) {
      setNow(Date.now());
      setLockedUntil(Date.now() + answer.lockedForSecs * 1000);
    }
    setError(answer.error);
  }

  const passkeyLink = (
    <button
      type="button"
      className="type-caption text-fg-muted underline-offset-2 hover:text-fg hover:underline"
      onClick={() => {
        onClose();
        openUnlockModal();
      }}
    >
      Owner? Use the passkey instead
    </button>
  );

  return (
    <Modal open onClose={onClose} label="Sign in" width="lg:max-w-[400px]" locked={busy}>
      {step === 'phone' ? (
        <form
          className="flex flex-col gap-4 p-6"
          onSubmit={(e) => {
            e.preventDefault();
            submitPhone();
          }}
        >
          <h2 className="type-title text-fg">Sign in</h2>
          <p className="type-body text-fg-muted">{reason ?? 'Sign in to trade on paper with your own portfolio.'} A code comes to your WhatsApp.</p>
          <label className="flex flex-col gap-1.5">
            <span className="type-label text-fg-muted">Mobile number</span>
            <span className="relative flex items-center">
              <span className="pointer-events-none absolute left-3 type-body text-fg-muted">+91</span>
              <input
                type="tel"
                inputMode="numeric"
                autoComplete="tel-national"
                aria-label="Mobile number"
                data-autofocus
                maxLength={14}
                value={typed}
                disabled={busy || locked}
                onChange={(e) => {
                  setTyped(e.target.value.replace(/[^\d\s+-]/g, ''));
                  setError(null);
                }}
                className={`${INPUT} pl-12 num-medium tracking-[0.08em]`}
              />
            </span>
          </label>
          {locked ? <p className="type-caption text-loss tnum">Too many attempts. Try again in {mmss(lockedFor)}.</p> : error && <p className="type-caption text-loss">{error}</p>}
          <div className="flex items-center gap-2">
            {passkeyLink}
            <span className="flex-1" />
            <Button size="md" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button size="md" kind="profit" type="submit" disabled={busy || locked || typed.replace(/\D/g, '').length < 10}>
              {busy ? 'Sending…' : 'Send code'}
            </Button>
          </div>
        </form>
      ) : (
        <form
          className="flex flex-col gap-4 p-6"
          onSubmit={(e) => {
            e.preventDefault();
            void submitCode(code);
          }}
        >
          <h2 className="type-title text-fg">Enter the code</h2>
          <p className="type-body text-fg-muted">
            Sent on WhatsApp to +91 {phone ? formatPhone(phone) : ''}.{' '}
            <button
              type="button"
              className="text-fg underline-offset-2 hover:underline"
              onClick={() => {
                setStep('phone');
                setError(null);
                setCode('');
              }}
            >
              Change number
            </button>
          </p>
          <input
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            aria-label="Code"
            data-autofocus
            maxLength={6}
            value={code}
            disabled={busy || locked}
            onChange={(e) => {
              const next = e.target.value.replace(/\D/g, '').slice(0, 6);
              setCode(next);
              setError(null);
              if (next.length === 6) void submitCode(next);
            }}
            className={`${INPUT} text-center num-large tracking-[0.6em]`}
          />
          {locked ? <p className="type-caption text-loss tnum">Too many attempts. Try again in {mmss(lockedFor)}.</p> : error && <p className="type-caption text-loss">{error}</p>}
          <div className="flex items-center gap-2">
            <Button size="sm" kind="quiet" disabled={busy || locked || resendIn > 0} onClick={() => phone && void sendCode(phone)}>
              {resendIn > 0 ? `Send again in ${resendIn}s` : 'Send again'}
            </Button>
            <span className="flex-1" />
            <Button size="md" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button size="md" kind="profit" type="submit" disabled={busy || locked || code.length !== 6}>
              {busy ? 'Checking…' : 'Sign in'}
            </Button>
          </div>
        </form>
      )}
    </Modal>
  );
}
