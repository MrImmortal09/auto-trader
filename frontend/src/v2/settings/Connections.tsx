import { useState } from 'react';
import { getKotak, getVersion, kotakAutoLogin } from '../lib/api';
import { plural } from '../lib/format';
import { usePoll } from '../lib/usePoll';
import type { HealthSet, Level, SystemAnswer } from '../lib/types';
import { useGuard } from '../shell/useGuard';
import { Button } from '../ui/Button';
import { useToast } from '../ui/toast';
import { KotakDisconnectDialog, ServerDialog } from './dialogs';
import { Row, Section, StateDot } from './parts';

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** "14 Mar 2027". */
function longDate(day: string): string {
  return `${Number(day.slice(8, 10))} ${MONTHS[Number(day.slice(5, 7)) - 1]} ${day.slice(0, 4)}`;
}

/** The Kotak login form isn't in v2 yet: it stays in the old design, at /v1/. */
const V1_SETTINGS = '/v1/settings';

/**
 * Connections: the server, Kotak, Telegram and Upstox — each with its
 * state, and what can be done about it from here.
 */
export function Connections({ serverBase, health, system, id, title }: { serverBase: string; health: HealthSet | null; system: SystemAnswer | null; id?: string; title?: string }) {
  const version = usePoll(() => getVersion(serverBase), 60_000, serverBase);
  const kotak = usePoll(() => getKotak(serverBase), 10_000, serverBase);
  const guard = useGuard();
  const toast = useToast();
  const [dialog, setDialog] = useState<'server' | 'disconnect' | null>(null);
  const [connecting, setConnecting] = useState(false);
  const k = kotak.data;
  const sys = system;
  const dot = (key: keyof HealthSet): Level => health?.[key]?.level ?? 'off';

  async function reconnect() {
    setConnecting(true);
    const sent = await guard('Logging in to Kotak needs write access.', () => kotakAutoLogin(serverBase), "Couldn't log in to Kotak");
    setConnecting(false);
    if (sent?.ok) toast({ tone: 'profit', title: 'Kotak connected' });
    await kotak.refresh();
  }

  const kotakLine = k
    ? [
        k.masked_ucc ? `UCC ${k.masked_ucc}` : null,
        k.auto_login_ready ? 'auto-login at 09:05 on trading days' : 'no auto-login',
        k.has_totp_secret ? 'TOTP secret saved' : null,
      ]
        .filter(Boolean)
        .join(' · ')
    : kotak.error ?? 'Checking…';
  const kotakState: [Level, string] = !k ? ['off', '…'] : k.connected ? ['ok', 'Connected'] : k.session_present ? ['down', 'Session failing'] : [dot('kotak') === 'down' ? 'down' : 'off', 'Not connected'];
  const listening = sys?.telegram.listening;
  const telegramLine = health?.telegram.level === 'ok' ? (listening != null ? `Listening to ${plural(listening, 'channel')}` : 'Listening') : 'Not listening';
  const upstoxLine = sys?.upstox.configured
    ? `Analytics token for charts, option chain and VWAP${sys.upstox.token_expires ? ` · valid till ${longDate(sys.upstox.token_expires)}` : ''}`
    : 'Not set up · market data needs UPSTOX_ANALYTICS_TOKEN on the server';

  return (
    <Section id={id} title={title}>
      <Row title="Server" caption={`${serverBase || window.location.origin}${version.data ? ` · backend v${version.data.version}` : ''}`}>
        <StateDot level={version.error ? 'down' : version.data ? 'ok' : 'off'}>{version.error ? "Can't reach" : 'Connected'}</StateDot>
        <Button size="sm" onClick={() => setDialog('server')}>
          Change
        </Button>
      </Row>
      <Row title="Kotak Neo" caption={kotakLine}>
        <StateDot level={kotakState[0]}>{kotakState[1]}</StateDot>
        {k?.has_env_credentials ? (
          <Button size="sm" onClick={() => void reconnect()} disabled={connecting}>
            {connecting ? 'Logging in…' : k.connected ? 'Reconnect' : 'Connect'}
          </Button>
        ) : (
          <a href={V1_SETTINGS} className="inline-flex h-7 items-center rounded-6 bg-raised px-2.5 type-button text-fg hover:bg-line">
            Log in
          </a>
        )}
        {k?.session_present && (
          <Button size="sm" kind="ghost" onClick={() => setDialog('disconnect')}>
            Disconnect
          </Button>
        )}
      </Row>
      <Row title="Telegram" caption={telegramLine}>
        <StateDot level={dot('telegram')}>{health?.telegram.level === 'ok' ? 'Connected' : 'Not running'}</StateDot>
        <a href={V1_SETTINGS} className="inline-flex h-7 items-center rounded-6 bg-raised px-2.5 type-button text-fg hover:bg-line">
          Manage channels
        </a>
      </Row>
      <Row title="Upstox" caption={upstoxLine}>
        <StateDot level={sys?.upstox.configured ? dot('upstox') : 'off'}>
          {!sys?.upstox.configured ? 'Not set up' : health?.upstox.level === 'ok' ? 'Connected' : health?.upstox.label ?? 'Set up'}
        </StateDot>
      </Row>
      {dialog === 'server' && <ServerDialog current={serverBase} onClose={() => setDialog(null)} />}
      {dialog === 'disconnect' && <KotakDisconnectDialog serverBase={serverBase} onClose={() => setDialog(null)} onDone={() => void kotak.refresh()} />}
    </Section>
  );
}
