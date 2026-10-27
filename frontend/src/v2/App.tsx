import { lazy, Suspense, useEffect, useState } from 'react';
import { Admin } from './admin/Admin';
import { useIdentity } from './auth/identity';
import { SignInSheet } from './auth/SignInSheet';
import { Desk } from './desk/Desk';
import { resumeTrading, streamToken } from './lib/api';
import { plural } from './lib/format';
import { useIsDesktop, useServerBase } from './lib/hooks';
import type { Stopped } from './lib/types';
import { StatusBanners } from './shell/Banners';
import { KillSwitchDialog } from './shell/KillSwitchDialog';
import { NavRail, TabBar } from './shell/Nav';
import { SCREENS, screenFromHash, TAB_SCREENS, visibleScreens, type ScreenId } from './shell/screens';
import { DesktopStatusBar, PhoneStatusBar } from './shell/StatusBar';
import { UnlockDialog } from './shell/UnlockDialog';
import { useGuard } from './shell/useGuard';
import { useEngineErrors, useStatus } from './shell/useStatus';
import { Banner } from './ui/Banner';
import { Settings } from './settings/Settings';
import { UserSettings } from './settings/UserSettings';
import { System } from './system/System';
import { Segment } from './ui/Segment';

// The charts are heavy (KLineChart): only loaded once Trade or Market is opened.
const Trade = lazy(() => import('./trade/Trade').then((m) => ({ default: m.Trade })));
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

/**
 * The v2 shell: status bar, banners, navigation and the current screen.
 * Anyone may look; what shows depends on who is signed in (`useIdentity`):
 * the owner and the admin get the whole desk, a user or a visitor the
 * public screens and their own sign-in.
 */
export default function App() {
  const serverBase = useServerBase();
  const desktop = useIsDesktop();
  const { role, canReadOwner } = useIdentity();
  const screens = visibleScreens(role);
  const [wanted, setWanted] = useState<ScreenId>(screenFromHash);
  // A screen this person may not open (an old hash, or a sign-out) falls back to Desk.
  const screen: ScreenId = screens.includes(wanted) ? wanted : 'desk';
  const [killSwitchOpen, setKillSwitchOpen] = useState(false);
  const [resuming, setResuming] = useState(false);
  const status = useStatus(serverBase, canReadOwner);
  const guard = useGuard();
  useEngineErrors(serverBase, streamToken(), canReadOwner);

  // The screen is mirrored in the hash (`/v2/#trade`): there are no deep paths.
  useEffect(() => {
    const onHash = () => setWanted(screenFromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  function go(next: ScreenId) {
    setWanted(next);
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
  const stopped = canReadOwner && s?.kill_switch ? s.stopped ?? { at: null, positions_exited: 0, entries_cancelled: 0 } : null;
  // Phone: the screens behind More, with a switch between them when there are several.
  const more = screens.filter((id) => !TAB_SCREENS.includes(id));

  const content =
    screen === 'desk' ? (
      <Desk serverBase={serverBase} desktop={desktop} stopped={stopped} role={role} />
    ) : screen === 'trade' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-muted">Loading charts…</p>}>
        <Trade serverBase={serverBase} desktop={desktop} />
      </Suspense>
    ) : screen === 'market' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-muted">Loading charts…</p>}>
        <Market serverBase={serverBase} desktop={desktop} canReadOwner={canReadOwner} />
      </Suspense>
    ) : (
      (() => {
        const page =
          screen === 'system' ? (
            <System serverBase={serverBase} desktop={desktop} health={s?.health ?? null} marketOpen={s?.market_open ?? false} />
          ) : screen === 'admin' ? (
            <Admin serverBase={serverBase} desktop={desktop} />
          ) : canReadOwner ? (
            <Settings
              serverBase={serverBase}
              desktop={desktop}
              health={s?.health ?? null}
              onKillSwitch={() => setKillSwitchOpen(true)}
              onModeChanged={() => void status.refresh()}
            />
          ) : (
            <UserSettings serverBase={serverBase} desktop={desktop} />
          );
        if (desktop || more.length < 2) return page;
        return (
          <div className="flex min-h-full flex-col">
            <div className="px-4 pt-4">
              <Segment
                label="More"
                size="lg"
                stretch
                value={screen}
                onChange={go}
                options={SCREENS.filter((sc) => more.includes(sc.id)).map((sc) => ({ value: sc.id, label: sc.label }))}
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
        <DesktopStatusBar status={s} unreachable={unreachable} role={role} onKillSwitch={() => setKillSwitchOpen(true)} />
      ) : (
        <PhoneStatusBar status={s} unreachable={unreachable} role={role} onKillSwitch={() => setKillSwitchOpen(true)} />
      )}
      {stopped && (
        <Banner
          kind="stopped"
          title={stopped.at ? `Trading stopped at ${stopped.at}` : 'Trading stopped'}
          body={stoppedLine(stopped)}
          action={{ label: resuming ? 'Resuming…' : 'Resume trading', onClick: () => void resume(), disabled: resuming }}
        />
      )}
      <StatusBanners serverBase={serverBase} status={s} canReadOwner={canReadOwner} />
      <div className="flex min-h-0 flex-1">
        {desktop && <NavRail screens={screens} screen={screen} onSelect={go} />}
        <main className="min-w-0 flex-1 overflow-y-auto">{content}</main>
      </div>
      {!desktop && <TabBar screens={screens} screen={screen} onSelect={go} />}

      {killSwitchOpen && canReadOwner && (
        <KillSwitchDialog serverBase={serverBase} mode={s?.mode} onClose={() => setKillSwitchOpen(false)} onDone={() => void status.refresh()} />
      )}
      <SignInSheet />
      <UnlockDialog serverBase={serverBase} />
    </div>
  );
}
