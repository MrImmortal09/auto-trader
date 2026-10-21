import { lazy, Suspense, useEffect, useState } from 'react';
import { Desk } from './desk/Desk';
import { resumeTrading } from './lib/api';
import { plural } from './lib/format';
import { useIsDesktop, useServerBase } from './lib/hooks';
import type { Stopped } from './lib/types';
import { StatusBanners } from './shell/Banners';
import { KillSwitchDialog } from './shell/KillSwitchDialog';
import { NavRail, TabBar } from './shell/Nav';
import { screenFromHash, type ScreenId } from './shell/screens';
import { DesktopStatusBar, PhoneStatusBar } from './shell/StatusBar';
import { UnlockDialog } from './shell/UnlockDialog';
import { useGuard } from './shell/useGuard';
import { useEngineErrors, useStatus } from './shell/useStatus';
import { Banner } from './ui/Banner';
import { Settings } from './settings/Settings';
import { System } from './system/System';
import { Segment } from './ui/Segment';

// The charts are heavy (KLineChart): only loaded once Scalp or Market is opened.
const Scalp = lazy(() => import('./scalp/Scalp').then((m) => ({ default: m.Scalp })));
const Market = lazy(() => import('./market/Market').then((m) => ({ default: m.Market })));

/** "Kill switch: 3 positions exited and the waiting call cancelled." */
function stoppedLine(s: Stopped): string {
  const did: string[] = [];
  if (s.positions_exited > 0) did.push(`${plural(s.positions_exited, 'position')} exited`);
  if (s.entries_cancelled === 1) did.push('the waiting call cancelled');
  if (s.entries_cancelled > 1) did.push(`${s.entries_cancelled} waiting calls cancelled`);
  const head = did.length ? `Kill switch: ${did.join(' and ')}.` : 'The kill switch is on.';
  return `${head} New calls are ignored until you resume.`;
}

/** The v2 shell: status bar, banners, navigation and the current screen. */
export default function App() {
  const serverBase = useServerBase();
  const desktop = useIsDesktop();
  const [screen, setScreen] = useState<ScreenId>(screenFromHash);
  const [killSwitchOpen, setKillSwitchOpen] = useState(false);
  const [resuming, setResuming] = useState(false);
  const status = useStatus(serverBase);
  const guard = useGuard();
  useEngineErrors(serverBase);

  // The screen is mirrored in the hash (`/v2/#scalp`): there are no deep paths.
  useEffect(() => {
    const onHash = () => setScreen(screenFromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  function go(next: ScreenId) {
    setScreen(next);
    if (window.location.hash !== `#${next}`) window.location.hash = next;
  }

  async function resume() {
    setResuming(true);
    await guard('Resuming trading needs write access.', () => resumeTrading(serverBase), "Couldn't resume trading");
    await status.refresh();
    setResuming(false);
  }

  const s = status.data;
  const unreachable = !!status.error;
  const live = s?.mode === 'LIVE';
  const stopped = s?.kill_switch ? s.stopped ?? { at: null, positions_exited: 0, entries_cancelled: 0 } : null;

  const content =
    screen === 'desk' ? (
      <Desk serverBase={serverBase} desktop={desktop} stopped={stopped} />
    ) : screen === 'scalp' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-muted">Loading charts…</p>}>
        <Scalp serverBase={serverBase} desktop={desktop} />
      </Suspense>
    ) : screen === 'market' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-muted">Loading charts…</p>}>
        <Market serverBase={serverBase} desktop={desktop} />
      </Suspense>
    ) : (
      (() => {
        const page =
          screen === 'system' ? (
            <System serverBase={serverBase} desktop={desktop} health={s?.health ?? null} />
          ) : (
            <Settings
              serverBase={serverBase}
              desktop={desktop}
              health={s?.health ?? null}
              onKillSwitch={() => setKillSwitchOpen(true)}
              onModeChanged={() => void status.refresh()}
            />
          );
        if (desktop) return page;
        // Phone: More holds Settings and System behind a two-way switch.
        return (
          <div className="flex min-h-full flex-col">
            <div className="px-4 pt-4">
              <Segment
                label="More"
                size="lg"
                stretch
                value={screen}
                onChange={go}
                options={[
                  { value: 'settings', label: 'Settings' },
                  { value: 'system', label: 'System' },
                ]}
              />
            </div>
            <div className="flex-1">{page}</div>
          </div>
        );
      })()
    );

  return (
    <div className="flex h-dvh flex-col overflow-hidden bg-canvas text-fg">
      {desktop ? (
        <DesktopStatusBar status={s} unreachable={unreachable} onKillSwitch={() => setKillSwitchOpen(true)} />
      ) : (
        <PhoneStatusBar status={s} unreachable={unreachable} onKillSwitch={() => setKillSwitchOpen(true)} />
      )}
      {stopped && (
        <Banner
          kind="stopped"
          title={stopped.at ? `Trading stopped at ${stopped.at}` : 'Trading stopped'}
          body={stoppedLine(stopped)}
          action={{ label: resuming ? 'Resuming…' : 'Resume trading', onClick: () => void resume(), disabled: resuming }}
        />
      )}
      <StatusBanners serverBase={serverBase} status={s} />
      <div className="flex min-h-0 flex-1">
        {desktop && <NavRail screen={screen} onSelect={go} />}
        <main className="min-w-0 flex-1 overflow-y-auto">{content}</main>
      </div>
      {!desktop && <TabBar screen={screen} onSelect={go} />}

      {/* Real money: the whole frame is edged in marigold. */}
      {live && <div aria-hidden className="pointer-events-none fixed inset-0 z-40 border-2 border-live" />}

      {killSwitchOpen && (
        <KillSwitchDialog serverBase={serverBase} mode={s?.mode} onClose={() => setKillSwitchOpen(false)} onDone={() => void status.refresh()} />
      )}
      <UnlockDialog serverBase={serverBase} />
    </div>
  );
}
