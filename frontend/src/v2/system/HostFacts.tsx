import { useEffect, useState } from 'react';
import { RefreshCw } from 'lucide-react';
import type { HealthSnapshot } from '../../types';
import { apiFetch } from '../../lib/api';
import { getKotak } from '../lib/api';
import type { KotakState } from '../lib/types';
import { Button } from '../ui/Button';
import { Card, CountRow } from './parts';
import { uptime } from './units';

interface Facts {
  host: HealthSnapshot | null;
  kotak: KotakState | null;
  error: string | null;
}

async function loadFacts(serverBase: string): Promise<Facts> {
  const [host, kotak] = await Promise.all([
    apiFetch(serverBase, '/api/health')
      .then((r) => (r.ok ? (r.json() as Promise<HealthSnapshot>) : null))
      .catch(() => null),
    getKotak(serverBase).catch(() => null),
  ]);
  return { host, kotak, error: host ? null : "Couldn't read the host's details." };
}

/**
 * The host and the configuration checks behind it — read when the screen
 * opens and on Refresh, not streamed: they change with deploys, not by the
 * second (`/api/health` and `/api/auth/kotak`).
 */
export function HostFacts({ serverBase }: { serverBase: string }) {
  const [facts, setFacts] = useState<Facts | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let live = true;
    loadFacts(serverBase).then((f) => {
      if (!live) return;
      setFacts(f);
      setLoading(false);
    });
    return () => {
      live = false;
    };
  }, [serverBase]);

  async function refresh() {
    setLoading(true);
    setFacts(await loadFacts(serverBase));
    setLoading(false);
  }

  const h = facts?.host;
  const k = facts?.kotak;
  return (
    <>
      <Card
        title="Host"
        aside={
          <Button size="sm" kind="ghost" icon={<RefreshCw size={13} className={loading ? 'animate-spin' : ''} />} onClick={() => void refresh()} disabled={loading}>
            Refresh
          </Button>
        }
      >
        {h && !h.auth_secret_configured && (
          <p className="mb-2 rounded-8 bg-loss-subtle px-3 py-2 type-caption text-loss">AUTH_SECRET isn't set on the server: every action that needs the passkey fails until it is.</p>
        )}
        {h ? (
          <>
            <CountRow label="Hostname" value={h.hostname ?? '—'} />
            <CountRow label="System" value={[h.os_name, h.os_version].filter(Boolean).join(' ') || '—'} />
            <CountRow label="Kernel" value={h.kernel_version ?? '—'} />
            <CountRow label="Host uptime" value={uptime(h.uptime_secs)} />
            <CountRow label="CPU cores" value={h.cpu_cores} />
            <CountRow label="Read at" value={h.generated_at_ist.slice(11, 19)} />
          </>
        ) : (
          <p className="py-2 type-caption text-fg-muted">{loading ? 'Reading…' : facts?.error}</p>
        )}
      </Card>
      <Card title="Kotak auto-login" note={k ? (k.auto_login_ready ? 'Ready' : 'Not ready') : undefined}>
        {k ? (
          <>
            <CountRow label="Auto-login" hint="KOTAK_AUTO_LOGIN" value={k.auto_login_enabled ? 'On' : 'Off'} />
            <CountRow label="TOTP secret" value={k.has_totp_secret ? 'Saved' : 'Missing'} alarm={!k.has_totp_secret} />
            <CountRow label="Every KOTAK_ setting" value={k.has_env_credentials ? 'Set' : 'Missing'} alarm={!k.has_env_credentials} />
            <CountRow label="Session" value={k.connected ? 'Connected' : 'Not connected'} />
            {!k.auto_login_ready && k.auto_login_reason && <p className="mt-1 type-caption text-loss">{k.auto_login_reason}</p>}
          </>
        ) : (
          <p className="py-2 type-caption text-fg-muted">{loading ? 'Reading…' : "Couldn't read Kotak's status."}</p>
        )}
      </Card>
    </>
  );
}
