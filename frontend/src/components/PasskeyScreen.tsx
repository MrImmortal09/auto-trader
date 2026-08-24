import { useEffect, useState, useRef } from 'react';
import type { KeyboardEvent } from 'react';
import { ShieldAlert, ShieldCheck, Loader2, AlertTriangle, Server, Pencil } from 'lucide-react';
import { apiFetch, getStoredServerBase, persistServerBase, normalizeServerBase, isValidServerBase } from '../lib/api';
import { setToken } from '../lib/auth';

interface PasskeyScreenProps {
  onSuccess: () => void;
}

export function PasskeyScreen({ onSuccess }: PasskeyScreenProps) {
  const [passkey, setPasskey] = useState<string[]>(Array(6).fill(''));
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [lockoutSecs, setLockoutSecs] = useState(0);
  const [shake, setShake] = useState(false);
  const [authSecretMissing, setAuthSecretMissing] = useState(false);
  const [serverBase, setServerBase] = useState(() => getStoredServerBase());
  const [serverBaseInput, setServerBaseInput] = useState(serverBase);
  const [serverBaseError, setServerBaseError] = useState('');
  const [serverUnreachable, setServerUnreachable] = useState(false);
  const [editingServer, setEditingServer] = useState(false);
  const inputRefs = useRef<(HTMLInputElement | null)[]>([]);

  function commitServerBase(rawValue: string) {
    const normalized = normalizeServerBase(rawValue);
    if (normalized && !isValidServerBase(normalized)) {
      setServerBaseError('Enter a full http:// or https:// server URL');
      return;
    }
    setServerBaseError('');
    persistServerBase(normalized);
    setServerBaseInput(normalized);
    setServerBase(normalized);
  }

  // /api/health is the one route that stays reachable even when AUTH_SECRET
  // is unset (auth_middleware 500s every other authenticated route in that
  // case) — check it proactively so a misconfigured backend shows a clear
  // reason instead of a login form that can never succeed. Re-runs whenever
  // the server URL below is edited, so switching servers gives instant
  // feedback without needing to submit a passkey first.
  useEffect(() => {
    setServerUnreachable(false);
    apiFetch(serverBase, '/api/health')
      .then((r) => r.json())
      .then((data) => setAuthSecretMissing(data?.auth_secret_configured === false))
      .catch(() => setServerUnreachable(true));
  }, [serverBase]);

  const triggerShake = () => {
    setShake(true);
    setTimeout(() => setShake(false), 500);
  };

  const handleChange = (index: number, val: string) => {
    if (lockoutSecs > 0) return;
    const char = val.slice(-1);
    if (!/^\d*$/.test(char)) return; // only digits

    const newPasskey = [...passkey];
    newPasskey[index] = char;
    setPasskey(newPasskey);
    setError('');

    if (char && index < 5) {
      inputRefs.current[index + 1]?.focus();
    }

    if (newPasskey.every(d => d !== '')) {
      verifyPasskey(newPasskey.join(''));
    }
  };

  const handleKeyDown = (index: number, e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Backspace' && !passkey[index] && index > 0) {
      inputRefs.current[index - 1]?.focus();
    }
  };

  const verifyPasskey = async (code: string) => {
    setLoading(true);
    try {
      const res = await apiFetch(serverBase, '/api/auth/verify-passkey', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ passkey: code }),
      });

      if (res.ok) {
        const data = await res.json();
        setToken(data.token);
        onSuccess();
      } else if (res.status === 429) {
        const retryAfter = parseInt(res.headers.get('Retry-After') || '900', 10);
        setLockoutSecs(retryAfter);
        setError('Too many attempts. Locked out.');
        triggerShake();
        startLockoutTimer(retryAfter);
      } else if (res.status === 500) {
        // The most likely cause is a missing AUTH_SECRET (verify_passkey_handler
        // panics without one) — not a wrong code, so don't say "invalid passkey".
        setError(authSecretMissing ? 'Backend misconfigured — see below' : 'Server error — check backend logs');
        setPasskey(Array(6).fill(''));
        triggerShake();
      } else {
        setError('Invalid passkey');
        setPasskey(Array(6).fill(''));
        inputRefs.current[0]?.focus();
        triggerShake();
      }
    } catch {
      setError('Connection error');
      triggerShake();
    } finally {
      setLoading(false);
    }
  };

  const startLockoutTimer = (initialSecs: number) => {
    let left = initialSecs;
    const interval = setInterval(() => {
      left -= 1;
      setLockoutSecs(left);
      if (left <= 0) {
        clearInterval(interval);
        setError('');
        setPasskey(Array(6).fill(''));
        inputRefs.current[0]?.focus();
      }
    }, 1000);
  };

  return (
    <div className="fixed inset-0 bg-surface/90 backdrop-blur-sm z-50 flex items-center justify-center p-4">
      <div className={`bg-surface-container-lowest border border-outline-variant rounded-3xl p-8 sm:p-12 shadow-2xl max-w-md w-full flex flex-col items-center text-center transition-transform ${shake ? 'animate-shake' : ''}`}>
        <div className="bg-primary/10 p-4 rounded-full mb-6">
          {lockoutSecs > 0 ? (
            <ShieldAlert size={48} className="text-error" />
          ) : (
            <ShieldCheck size={48} className="text-primary" />
          )}
        </div>
        
        <h2 className="text-2xl font-bold text-on-surface mb-2">Secure Access</h2>
        <p className="text-on-surface-variant mb-6">Enter your 6-digit passkey to connect to the trading engine.</p>

        <div className="w-full mb-6">
          {editingServer ? (
            <div className="flex flex-col gap-1.5">
              <input
                type="text"
                autoFocus
                value={serverBaseInput}
                placeholder="http://host:8080 or https://your-domain"
                onChange={(e) => setServerBaseInput(e.target.value)}
                onBlur={(e) => { commitServerBase(e.target.value); setEditingServer(false); }}
                onKeyDown={(e) => { if (e.key === 'Enter') { commitServerBase(serverBaseInput); setEditingServer(false); } }}
                className="w-full bg-surface border border-outline-variant rounded-lg px-3 py-2 text-sm text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary font-mono-code"
              />
              {serverBaseError && <span className="text-xs text-error text-left">{serverBaseError}</span>}
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setEditingServer(true)}
              className={`flex items-center gap-2 w-full text-left text-xs px-3 py-2 rounded-lg border transition-colors ${
                serverUnreachable
                  ? 'border-error text-error bg-error-container/30 hover:bg-error-container/50'
                  : 'border-outline-variant text-on-surface-variant hover:border-primary hover:text-on-surface'
              }`}
            >
              <Server size={14} className="shrink-0" />
              <span className="truncate font-mono-code">{serverBase || 'No server configured'}</span>
              <Pencil size={12} className="shrink-0 ml-auto opacity-60" />
            </button>
          )}
          {serverUnreachable && !editingServer && (
            <p className="text-xs text-error text-left mt-1.5">Can't reach this server — click above to change it.</p>
          )}
        </div>

        {authSecretMissing && (
          <div className="flex items-start gap-2 text-left text-sm text-error bg-error-container/40 border border-error rounded-xl px-4 py-3 mb-6">
            <AlertTriangle size={18} className="shrink-0 mt-0.5" />
            <span>
              Backend has no <code className="font-mono-code">AUTH_SECRET</code> configured — login will fail no matter
              what code you enter. Set it in the backend's environment and restart the server.
            </span>
          </div>
        )}

        <div className="flex gap-2 sm:gap-3 mb-6" dir="ltr">
          {passkey.map((digit, i) => (
            <input
              key={i}
              ref={el => { inputRefs.current[i] = el; }}
              type="text"
              inputMode="numeric"
              pattern="[0-9]*"
              maxLength={1}
              value={digit}
              disabled={loading || lockoutSecs > 0}
              onChange={e => handleChange(i, e.target.value)}
              onKeyDown={e => handleKeyDown(i, e)}
              className="w-10 h-12 sm:w-12 sm:h-14 text-center text-xl sm:text-2xl font-bold bg-surface rounded-xl border border-outline focus:border-primary focus:ring-2 focus:ring-primary/20 outline-none transition-all disabled:opacity-50"
            />
          ))}
        </div>

        {error && (
          <div className="text-error font-medium mb-4 animate-in fade-in slide-in-from-bottom-2">
            {error} {lockoutSecs > 0 && `(${Math.floor(lockoutSecs / 60)}m ${lockoutSecs % 60}s)`}
          </div>
        )}

        {loading && (
          <div className="flex items-center gap-2 text-primary">
            <Loader2 size={20} className="animate-spin" />
            <span>Verifying...</span>
          </div>
        )}
      </div>
    </div>
  );
}
