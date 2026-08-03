import { useEffect, useState } from 'react';
import { MessageCircle } from 'lucide-react';
import type { TelegramChat, TgStep } from '../types';
import { apiFetch } from '../lib/api';

export function TelegramLoginPanel({ serverBase }: { serverBase: string }) {
  const [step, setStep] = useState<TgStep>('idle');
  const [apiId, setApiId]     = useState(() => localStorage.getItem('tg_api_id') || '');
  const [apiHash, setApiHash] = useState(() => localStorage.getItem('tg_api_hash') || '');
  const [phone, setPhone]     = useState(() => localStorage.getItem('tg_phone') || '');
  const [code, setCode]       = useState('');
  const [twofa, setTwofa]     = useState(() => localStorage.getItem('tg_twofa') || '');
  const [chats, setChats]     = useState<TelegramChat[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [err, setErr]         = useState('');

  useEffect(() => {
    async function checkState() {
      try {
        const res = await apiFetch(serverBase, '/api/auth/telegram/status');
        if (res.ok) {
          const data = await res.json();
          if (data.state === 'running') {
            setStep('running');
            if (Array.isArray(data.chat_ids)) {
              setSelected(new Set(data.chat_ids));
            }
          }
          else if (data.state === 'authenticated') loadChats();
        }
      } catch (e) {}
    }
    checkState();
  }, [serverBase]);

  useEffect(() => { localStorage.setItem('tg_api_id', apiId); }, [apiId]);
  useEffect(() => { localStorage.setItem('tg_api_hash', apiHash); }, [apiHash]);
  useEffect(() => { localStorage.setItem('tg_phone', phone); }, [phone]);
  useEffect(() => { localStorage.setItem('tg_twofa', twofa); }, [twofa]);

  async function post(url: string, body: object) {
    const res = await apiFetch(serverBase, url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
    return res.json();
  }

  async function requestCode() {
    setErr('');
    const data = await post('/api/auth/telegram/request-code', {
      api_id: parseInt(apiId, 10), api_hash: apiHash, phone,
    });
    if (data.error) { setErr(data.error); return; }
    if (data.status === 'authenticated') {
      await loadChats();
    } else {
      setStep('code');
    }
  }

  async function submitCode() {
    setErr('');
    const data = await post('/api/auth/telegram/submit-code', { code });
    if (data.error) { setErr(data.error); return; }
    if (data.twofa_required) { setStep('twofa'); }
    else { await loadChats(); }
  }

  async function submit2fa() {
    setErr('');
    const data = await post('/api/auth/telegram/submit-2fa', { password: twofa });
    if (data.error) { setErr(data.error); return; }
    await loadChats();
  }

  async function loadChats() {
    const data: TelegramChat[] | { error: string } = await apiFetch(serverBase, '/api/auth/telegram/chats').then(r => r.json());
    if ('error' in data) { setErr(data.error); return; }
    setChats(data);
    setStep('chats');
  }

  async function startMonitoring() {
    setErr('');
    const data = await post('/api/auth/telegram/start', { chat_ids: [...selected] });
    if (data.error) { setErr(data.error); return; }
    setStep('running');
  }

  const kindIcon = (k: string) =>
    k === 'user' ? '👤' : k === 'channel' ? '📢' : k === 'community' ? '🏛' : '👥';

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2 text-label-caps font-semibold text-on-surface uppercase tracking-wider">
        <MessageCircle size={14} className="text-primary" /> Telegram Userbot
        {step === 'running' && <span className="text-secondary normal-case font-normal">(monitoring)</span>}
      </div>

      {/* Step: idle — enter credentials */}
      {step === 'idle' && (
        <div className="flex flex-wrap gap-2">
          <input value={apiId}   onChange={e => setApiId(e.target.value)}
            placeholder="API ID" className="w-24 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code" />
          <input value={apiHash} onChange={e => setApiHash(e.target.value)}
            placeholder="API Hash" className="w-52 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code" />
          <input value={phone}   onChange={e => setPhone(e.target.value)}
            placeholder="+91XXXXXXXXXX" className="w-36 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code" />
          <button onClick={requestCode}
            className="btn-sm bg-primary hover:bg-primary/90 text-on-primary font-semibold shadow-sm">
            Send Code
          </button>
        </div>
      )}

      {/* Step: code — enter the 5-digit code */}
      {step === 'code' && (
        <div className="flex flex-wrap gap-2 items-center">
          <span className="text-xs text-on-surface-variant">Code sent to {phone}:</span>
          <input value={code} onChange={e => setCode(e.target.value)}
            placeholder="12345" maxLength={10} className="w-24 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code" />
          <button onClick={submitCode} className="btn-sm bg-primary hover:bg-primary/90 text-on-primary font-semibold shadow-sm">
            Confirm
          </button>
          <button onClick={() => setStep('idle')} className="btn-sm bg-surface-container hover:bg-surface-container-high text-on-surface font-medium">
            Back
          </button>
        </div>
      )}

      {/* Step: 2FA */}
      {step === 'twofa' && (
        <div className="flex flex-wrap gap-2 items-center">
          <span className="text-xs text-on-surface-variant">2FA password:</span>
          <input value={twofa} type="password" onChange={e => setTwofa(e.target.value)}
            placeholder="password" className="w-36 bg-surface-container-lowest border border-outline-variant rounded px-2.5 py-1 text-xs text-on-surface placeholder-on-surface-variant focus:outline-none focus:border-primary shadow-sm font-mono-code" />
          <button onClick={submit2fa} className="btn-sm bg-primary hover:bg-primary/90 text-on-primary font-semibold shadow-sm">
            Confirm
          </button>
        </div>
      )}

      {/* Step: chats — multi-select which groups to monitor */}
      {step === 'chats' && (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-on-surface-variant">
            Select groups/channels to monitor ({selected.size} selected):
          </p>
          <div className="flex flex-wrap gap-1 max-h-28 overflow-y-auto pr-1">
            {chats.map(c => {
              const on = selected.has(c.id);
              return (
                <button
                  key={c.id}
                  onClick={() => {
                    setSelected(prev => {
                      const s = new Set(prev);
                      on ? s.delete(c.id) : s.add(c.id);
                      return s;
                    });
                  }}
                  className={`flex items-center gap-1 px-2.5 py-1 rounded-md text-xs border transition-colors ${
                    on
                      ? 'bg-primary border-primary text-on-primary shadow-sm'
                      : 'bg-surface-container-lowest border-outline-variant text-on-surface hover:border-primary'
                  }`}
                >
                  {kindIcon(c.kind)} {c.name}
                  <span className={`text-[10px] ${on ? 'text-on-primary/80' : 'text-on-surface-variant'}`}>({c.id})</span>
                </button>
              );
            })}
          </div>
          <div className="flex gap-2">
            <button
              onClick={startMonitoring}
              disabled={selected.size === 0}
              className="btn-sm bg-secondary hover:bg-secondary/90 text-on-secondary disabled:opacity-40 font-semibold shadow-sm"
            >
              Start Monitoring ({selected.size})
            </button>
            <button onClick={() => { setStep('idle'); setChats([]); setSelected(new Set()); }}
              className="btn-sm bg-surface-container hover:bg-surface-container-high text-on-surface font-medium">
              Reset
            </button>
          </div>
        </div>
      )}

      {/* Running state */}
      {step === 'running' && (
        <div className="flex items-center gap-3 text-xs">
          <span className="text-secondary font-semibold">● Monitoring {selected.size} chat(s)</span>
          <button onClick={() => { setStep('idle'); setSelected(new Set()); setChats([]); }}
            className="btn-sm bg-surface-container hover:bg-surface-container-high text-on-surface font-medium">
            Disconnect
          </button>
        </div>
      )}

      {err && <p className="text-xs text-error font-medium">{err}</p>}
    </div>
  );
}
