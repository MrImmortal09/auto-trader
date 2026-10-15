import { useEffect, useState } from 'react';
import { KeyRound, RefreshCw, Server } from 'lucide-react';
import type { HealthSnapshot, KotakStatus } from '../../types';
import { apiFetch } from '../../lib/api';
import { fmtUptime } from '../../lib/format';
import { Section, StatusBadge } from './PulseParts';

async function fetchHost(serverBase: string) {
  const [healthRes, kotakRes] = await Promise.all([
    apiFetch(serverBase, '/api/health'),
    apiFetch(serverBase, '/api/auth/kotak'),
  ]);
  const health = await healthRes.json();
  if (!healthRes.ok) throw new Error(health?.error ?? 'Failed to load host details');
  // Kotak status is a secondary diagnostic — never fail the panel over it.
  const kotak: KotakStatus | null = kotakRes.ok ? await kotakRes.json() : null;
  return { health: health as HealthSnapshot, kotak };
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <span className="text-on-surface-variant">{label}:</span>{' '}
      <span className="text-on-surface font-medium break-words">{value}</span>
    </div>
  );
}

/** Static host facts and configuration checks — fetched on open, not streamed
 * (they change on deploys, not by the second). */
export function HostPanel({ serverBase }: { serverBase: string }) {
  const [health, setHealth] = useState<HealthSnapshot | null>(null);
  const [kotak, setKotak] = useState<KotakStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  useEffect(() => {
    let cancelled = false;
    fetchHost(serverBase)
      .then((r) => {
        if (cancelled) return;
        setHealth(r.health);
        setKotak(r.kotak);
        setError('');
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [serverBase]);

  function refresh() {
    setLoading(true);
    fetchHost(serverBase)
      .then((r) => {
        setHealth(r.health);
        setKotak(r.kotak);
        setError('');
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  return (
    <div className="space-y-4">
      {health && !health.auth_secret_configured && (
        <div className="bg-error-container border border-error text-on-error-container rounded-lg px-4 py-3 text-sm">
          <span className="font-semibold">AUTH_SECRET is not set</span> on the backend — every authenticated API
          route will fail with 500 until it's configured and the server is restarted.
        </div>
      )}
      {error && (
        <div className="bg-error-container border border-error text-on-error-container rounded-lg px-4 py-3 text-sm">
          {error}
        </div>
      )}

      <div className="grid gap-4 lg:grid-cols-2">
        <Section
          icon={<Server size={15} className="text-primary" />}
          title="Host"
          aside={
            <button
              onClick={refresh}
              disabled={loading}
              className="flex items-center gap-1.5 px-2.5 py-1 text-xs font-semibold rounded-md border border-outline-variant text-on-surface hover:bg-surface-container disabled:opacity-50"
            >
              <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
              Refresh
            </button>
          }
        >
          {health ? (
            <div className="grid gap-2 sm:grid-cols-2 text-sm">
              <Row label="Hostname" value={health.hostname ?? '—'} />
              <Row label="OS" value={[health.os_name, health.os_version].filter(Boolean).join(' ') || '—'} />
              <Row label="Kernel" value={health.kernel_version ?? '—'} />
              <Row label="Host uptime" value={fmtUptime(health.uptime_secs)} />
              <Row label="CPU cores" value={String(health.cpu_cores)} />
              <Row label="Checked (IST)" value={health.generated_at_ist} />
            </div>
          ) : (
            <div className="text-sm text-on-surface-variant">{loading ? 'Loading…' : 'Host details unavailable.'}</div>
          )}
        </Section>

        <Section
          icon={<KeyRound size={15} className="text-primary" />}
          title="Kotak auto-login"
          aside={
            kotak && (
              <StatusBadge
                level={kotak.auto_login_ready ? 'good' : 'warning'}
                label={kotak.auto_login_ready ? 'Ready' : 'Not ready'}
              />
            )
          }
        >
          {kotak ? (
            <div className="space-y-2 text-sm">
              <div className="grid gap-2 sm:grid-cols-2">
                <Row label="KOTAK_AUTO_LOGIN" value={kotak.auto_login_enabled ? 'enabled' : 'disabled'} />
                <Row label="TOTP secret set" value={kotak.has_totp_secret ? 'Yes' : 'No'} />
                <Row label="All KOTAK_* fields set" value={kotak.has_env_credentials ? 'Yes' : 'No'} />
                <Row label="Session connected" value={kotak.connected ? 'Yes' : 'No'} />
              </div>
              {!kotak.auto_login_ready && kotak.auto_login_reason && (
                <div className="text-xs text-error">{kotak.auto_login_reason}</div>
              )}
            </div>
          ) : (
            <div className="text-sm text-on-surface-variant">{loading ? 'Loading…' : 'Kotak status unavailable.'}</div>
          )}
        </Section>
      </div>
    </div>
  );
}
