import { useEffect, useState } from 'react';
import { KeyRound, Plug, Zap } from 'lucide-react';
import type { KotakForm, KotakStatus } from '../types';
import { apiFetch, apiUrl, getStoredServerBase, isValidServerBase, normalizeServerBase } from '../lib/api';

export function KotakLoginPanel({ serverBase, onServerBaseChange }: {
  serverBase: string;
  onServerBaseChange: (value: string) => void;
}) {
  const [form, setForm] = useState<KotakForm>(() => {
    try {
      const saved = localStorage.getItem('kotak_creds');
      if (saved) {
        return {
          server_base: getStoredServerBase(),
          ...JSON.parse(saved),
          totp: '',
        } as KotakForm;
      }
    } catch {}
    return {
      server_base: getStoredServerBase(),
      access_token: '',
      mobile_number: '',
      ucc: '',
      totp: '',
      mpin: '',
    };
  });
  const [status, setStatus] = useState<'idle' | 'loading' | 'ok' | 'error'>('idle');
  const [msg, setMsg] = useState('');
  const [autoStatus, setAutoStatus] = useState<'idle' | 'loading' | 'error'>('idle');
  const [kotakStatus, setKotakStatus] = useState<KotakStatus | null>(null);

  function commitServerBase(rawValue: string) {
    const normalized = normalizeServerBase(rawValue);
    if (!normalized) {
      onServerBaseChange('');
      setMsg('');
      if (status !== 'loading') setStatus('idle');
      return true;
    }

    if (!isValidServerBase(normalized)) {
      setStatus('error');
      setMsg('Enter a full http:// or https:// server URL');
      return false;
    }

    onServerBaseChange(normalized);
    if (status === 'error' && msg === 'Enter a full http:// or https:// server URL') {
      setStatus('idle');
      setMsg('');
    }
    return true;
  }

  useEffect(() => {
    setForm((current) => current.server_base === serverBase ? current : { ...current, server_base: serverBase });
  }, [serverBase]);

  useEffect(() => {
    async function checkState() {
      if (!serverBase) return;
      try {
        const res = await apiFetch(serverBase, '/api/auth/kotak');
        if (res.ok) {
          const data: KotakStatus = await res.json();
          setKotakStatus(data);
          if (data.connected) {
            setStatus('ok');
            setMsg('Connected ✓');
          }
        }
      } catch (e) {}
    }
    checkState();
  }, [serverBase]);

  useEffect(() => {
    const { totp: _totp, server_base: _server_base, ...rest } = form;
    localStorage.setItem('kotak_creds', JSON.stringify(rest));
  }, [form]);

  async function handleLogin() {
    if (!commitServerBase(form.server_base)) return;
    setStatus('loading');
    try {
      const res = await apiFetch(form.server_base, '/api/auth/kotak', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(form),
      });
      // Try to parse the JSON body regardless of status so we can surface
      // the server's own error message when available.
      let data: { error?: string; status?: string } = {};
      try { data = await res.json(); } catch { /* non-JSON body, ignore */ }

      if (res.ok) {
        setStatus('ok');
        setMsg('Connected ✓');
      } else {
        setStatus('error');
        setMsg(data.error ?? `Server returned ${res.status} — check credentials`);
      }
    } catch (e) {
      setStatus('error');
      // `fetch` throws a TypeError("Failed to fetch") when there is no HTTP
      // response at all (network down, proxy timeout, CORS preflight blocked,
      // SSL error, etc.).  The raw error string is opaque to end-users, so
      // replace it with something actionable.
      const isNetworkError =
        e instanceof TypeError && /failed to fetch|network/i.test(e.message);
      setMsg(
        isNetworkError
          ? `Cannot reach server — verify Server URL and that the backend is running`
          : String(e),
      );
    }
  }

  async function handleAutoConnect() {
    if (!commitServerBase(form.server_base)) return;
    setAutoStatus('loading');
    setStatus('loading');
    try {
      const res = await apiFetch(form.server_base, '/api/auth/kotak/auto-login', { method: 'POST' });
      let data: { error?: string } = {};
      try { data = await res.json(); } catch { /* non-JSON body, ignore */ }

      if (res.ok) {
        setAutoStatus('idle');
        setStatus('ok');
        setMsg('Connected ✓ (auto)');
      } else {
        setAutoStatus('error');
        setStatus('error');
        setMsg(data.error ?? `Server returned ${res.status}`);
      }
    } catch (e) {
      setAutoStatus('error');
      setStatus('error');
      const isNetworkError = e instanceof TypeError && /failed to fetch|network/i.test(e.message);
      setMsg(isNetworkError ? `Cannot reach server — verify Server URL and that the backend is running` : String(e));
    }
  }

  const fields: { key: keyof typeof form; label: string; type?: string }[] = [
    { key: 'server_base',   label: 'Server URL or IP:PORT' },
    { key: 'access_token',  label: 'API Access Token' },
    { key: 'mobile_number', label: 'Mobile (+91…)' },
    { key: 'ucc',           label: 'UCC (Client Code)' },
    { key: 'totp',          label: kotakStatus?.has_totp_secret ? 'TOTP (auto-generated, or override)' : 'TOTP (6 digits)', type: 'text' },
    { key: 'mpin',          label: 'MPIN (6 digits)', type: 'password' },
  ];

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 text-label-caps font-semibold text-on-surface uppercase tracking-wider">
        <KeyRound size={14} className="text-primary" /> Kotak Neo Login
        {kotakStatus?.has_totp_secret && (
          <span className="flex items-center gap-1 normal-case tracking-normal text-[11px] font-medium text-secondary bg-secondary/10 rounded px-1.5 py-0.5">
            <Zap size={10} /> Auto-TOTP configured
          </span>
        )}
      </div>
      <div className="flex flex-wrap gap-2">
        {fields.map(({ key, label, type }) => (
          <input
            key={key}
            type={type ?? 'text'}
            placeholder={label}
            value={form[key]}
            onChange={e => setForm(f => ({ ...f, [key]: e.target.value }))}
            onBlur={key === 'server_base' ? (e) => { void commitServerBase(e.target.value); } : undefined}
            className="w-36 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code"
          />
        ))}
        <button
          onClick={handleLogin}
          disabled={status === 'loading' || status === 'ok'}
          className="flex items-center gap-1.5 px-3 py-1 bg-primary hover:bg-primary/90 disabled:opacity-50 text-on-primary text-xs font-semibold rounded transition-colors shadow-sm"
        >
          <Plug size={12} />
          {status === 'loading' && autoStatus !== 'loading' ? 'Connecting…' : status === 'ok' ? 'Connected' : 'Connect'}
        </button>
        {kotakStatus?.has_env_credentials && status !== 'ok' && (
          <button
            onClick={handleAutoConnect}
            disabled={status === 'loading'}
            title="Log in using the server's configured KOTAK_* env credentials — no fields required"
            className="flex items-center gap-1.5 px-3 py-1 bg-secondary hover:bg-secondary/90 disabled:opacity-50 text-on-secondary text-xs font-semibold rounded transition-colors shadow-sm"
          >
            <Zap size={12} />
            {autoStatus === 'loading' ? 'Connecting…' : 'Auto Connect'}
          </button>
        )}
        {msg && (
          <span className={`text-xs self-center ${status === 'ok' ? 'text-secondary font-medium' : 'text-error font-medium'}`}>
            {msg}
          </span>
        )}
        {status === 'ok' && (
          <div className="flex gap-2 ml-auto">
            <a href={apiUrl(form.server_base, '/api/auth/kotak/scrip-master/raw')} download="scrip_master.csv" className="text-[11px] text-primary hover:text-primary/80 underline self-center font-medium">Download CSV</a>
            <a href={apiUrl(form.server_base, '/api/auth/kotak/scrip-master/json')} target="_blank" rel="noreferrer" className="text-[11px] text-primary hover:text-primary/80 underline self-center font-medium">View JSON</a>
          </div>
        )}
      </div>
    </div>
  );
}
