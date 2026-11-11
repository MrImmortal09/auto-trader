import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { Admin } from './admin/Admin';
import { useIdentity } from './auth/identity';
import { SignInSheet } from './auth/SignInSheet';
import { Desk } from './desk/Desk';
import { Instrument } from './instrument/Instrument';
import { getFlags, resumeTrading, streamToken } from './lib/api';
import { plural } from './lib/format';
import { useIsDesktop, useServerBase } from './lib/hooks';
import { pointTradeAt, targetOf, type TradeIntent } from './lib/openInstrument';
import type { InstrumentRow, Stopped } from './lib/types';
import { usePoll } from './lib/usePoll';
import { StatusBanners } from './shell/Banners';
import { KillSwitchDialog } from './shell/KillSwitchDialog';
import { NavRail, TabBar } from './shell/Nav';
import { navScreens, SCREENS, screenFromLocation, screenPath, tabScreens, visibleScreens, type Place, type ScreenId } from './shell/screens';
import { Search, type OpenHow } from './shell/Search';
import { SearchSheet } from './shell/SearchSheet';
import { DesktopStatusBar, PhoneStatusBar } from './shell/StatusBar';
import { useGuard } from './shell/useGuard';
import { useEngineErrors, useStatus } from './shell/useStatus';
import { Banner } from './ui/Banner';
import { Settings } from './settings/Settings';
import { UserSettings } from './settings/UserSettings';
import { Logs } from './system/Logs';
import { System } from './system/System';
import { Segment } from './ui/Segment';
import { useNavHidden } from './lib/layout';
import { useAddToWatchlist } from './watchlist/AddToWatchlist';
import { Watchlist } from './watchlist/Watchlist';

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
 * Only for someone signed in (`SignInGate`); what shows depends on who
 * (`useIdentity`): the owner and the admin get the whole desk, a user the
 * public screens and their own sign-in.
 */
export default function App() {
  const serverBase = useServerBase();
  const desktop = useIsDesktop();
  // Desktop: the navigation rail folded away (the status bar's mode chip).
  const [navHidden] = useNavHidden();
  const { role, canReadOwner } = useIdentity();
  const flags = usePoll(() => getFlags(serverBase), 30_000, serverBase);
  // The instrument search is the admin's switch: off, the well and the tab are gone.
  const searchOn = flags.data?.search === true;
  // Watchlists too, and only while the search is on as well: with search
  // off the server prices no stock or contract (its quote poller skips
  // every tick), and the search and the instrument page, the only ways to
  // add one, are gone. Everything below reads this one value: the screen,
  // the rail and tabs, the "+" on a result or an instrument page, the
  // picker. `null` until the switches are known, so an opened
  // `/v2/watchlist` waits for them rather than bounce to Desk.
  const watchlistsOn: boolean | null = flags.data ? flags.data.watchlists === true && flags.data.search === true : null;
  // Any stock's chart, ladder and option chain (plan Phase 3): off, every
  // stock control stays hidden and a stock opens its instrument page alone.
  const stocksOn = flags.data?.stock_charts === true;
  const screens = visibleScreens(role, watchlistsOn !== false);
  const nav = navScreens(role, watchlistsOn === true);
  // Phone: the tabs with a place of their own (a user's start with Watchlist once it is on).
  const tabs = tabScreens(role, watchlistsOn === true);
  const [wanted, setWanted] = useState<Place>(screenFromLocation);
  // A screen this person may not open (an old link, or a sign-out) falls back to Desk.
  const screen: ScreenId = screens.includes(wanted.screen) ? wanted.screen : 'desk';
  const place: Place = screen === 'instrument' && wanted.key ? { screen, key: wanted.key } : { screen: screen === 'instrument' ? 'desk' : screen };
  const [killSwitchOpen, setKillSwitchOpen] = useState(false);
  const [resuming, setResuming] = useState(false);
  // Phone: the search sheet over everything.
  const [searchOpen, setSearchOpen] = useState(false);
  // Bumped when the search points the Trade screen somewhere, so an open
  // Trade re-reads its saved layout.
  const [tradeSeq, setTradeSeq] = useState(0);
  const status = useStatus(serverBase, canReadOwner);
  const guard = useGuard();
  // The "+" on a search result or an instrument page, and its picker.
  const addTo = useAddToWatchlist(serverBase, watchlistsOn === true);
  const onAdd = watchlistsOn === true ? addTo.add : undefined;
  useEngineErrors(serverBase, streamToken(), canReadOwner);

  // The screen is the address (`/v2/trade`): Back and Forward move between screens.
  useEffect(() => {
    const onPop = () => setWanted(screenFromLocation());
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);

  // `/`, an old `/#trade` link or a screen this person can't open: show the
  // address of the screen actually open (`/v2/i/<key>` for an instrument).
  const path = screenPath(place);
  useEffect(() => {
    if (window.location.pathname !== path || window.location.hash) {
      window.history.replaceState(null, '', path + window.location.search);
    }
  }, [path]);

  // Screens this page has pushed onto the history, so Back from an
  // instrument page knows whether there is a v2 screen behind it.
  const pushed = useRef(0);
  useEffect(() => {
    const onPop = () => {
      pushed.current = Math.max(0, pushed.current - 1);
    };
    window.addEventListener('popstate', onPop);
    return () => window.removeEventListener('popstate', onPop);
  }, []);

  function go(next: ScreenId | Place) {
    const p: Place = typeof next === 'string' ? { screen: next } : next;
    setWanted(p);
    if (window.location.pathname !== screenPath(p)) {
      window.history.pushState(null, '', screenPath(p));
      pushed.current++;
    }
  }

  /** Open the Trade screen on a chart and/or the ladder (from the search or
   * an instrument page). */
  function openTrade(intent: TradeIntent) {
    pointTradeAt(intent);
    setTradeSeq((n) => n + 1);
    go('trade');
  }

  /** A search result: Enter opens it (its chart, while stocks are on and the
   * Trade screen can draw one; its page otherwise), Shift+Enter puts it on
   * the ladder, the chart button charts it. */
  function openResult(row: InstrumentRow, how: OpenHow) {
    setSearchOpen(false);
    const target = targetOf(row, how, stocksOn);
    if ('trade' in target) openTrade(target.trade);
    else go({ screen: 'instrument', key: target.instrument });
  }

  /** Back from an instrument page: the screen before it, or Desk when the
   * page was opened from its address alone. */
  function back() {
    if (pushed.current > 0) window.history.back();
    else go('desk');
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
  const more = nav.filter((id) => !tabs.includes(id));

  const content =
    screen === 'desk' ? (
      <Desk serverBase={serverBase} desktop={desktop} stopped={stopped} role={role} />
    ) : screen === 'trade' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-subtle">Loading charts…</p>}>
        <Trade key={tradeSeq} serverBase={serverBase} desktop={desktop} />
      </Suspense>
    ) : screen === 'market' ? (
      <Suspense fallback={<p className="p-6 type-body text-fg-subtle">Loading charts…</p>}>
        <Market serverBase={serverBase} desktop={desktop} canReadOwner={canReadOwner} />
      </Suspense>
    ) : screen === 'instrument' && place.key ? (
      <Instrument key={place.key} serverBase={serverBase} desktop={desktop} id={place.key} onBack={back} onOpenTrade={openTrade} onAddToWatchlist={onAdd} stocksOn={stocksOn} />
    ) : screen === 'watchlist' && watchlistsOn == null ? (
      <p className="p-6 type-body text-fg-subtle">Loading…</p>
    ) : (
      (() => {
        const page =
          screen === 'watchlist' ? (
            <Watchlist serverBase={serverBase} desktop={desktop} stocksOn={stocksOn} onOpenTrade={openTrade} onOpenInstrument={(id) => go({ screen: 'instrument', key: id })} />
          ) : screen === 'system' ? (
            <System serverBase={serverBase} desktop={desktop} health={s?.health ?? null} marketOpen={s?.market_open ?? false} />
          ) : screen === 'logs' ? (
            <Logs serverBase={serverBase} desktop={desktop} />
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
        // A link-only screen (System) opens on its own, outside More's switch.
        if (desktop || more.length < 2 || !more.includes(screen)) return page;
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
        <DesktopStatusBar
          status={s}
          unreachable={unreachable}
          role={role}
          onKillSwitch={() => setKillSwitchOpen(true)}
          search={searchOn ? <Search serverBase={serverBase} onOpen={openResult} onAdd={onAdd} stocksOn={stocksOn} /> : undefined}
        />
      ) : (
        <PhoneStatusBar
          status={s}
          unreachable={unreachable}
          role={role}
          onKillSwitch={() => setKillSwitchOpen(true)}
          more={searchOn ? { screens: more, screen, onSelect: go } : undefined}
        />
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
        {desktop && !navHidden && <NavRail screens={nav} screen={screen} onSelect={go} />}
        {/* Trade fits the window and never scrolls: its ladder and panel scroll inside. */}
        <main className={`min-w-0 flex-1 ${screen === 'trade' ? 'overflow-hidden' : 'overflow-y-auto'}`}>{content}</main>
      </div>
      {!desktop && <TabBar screens={nav} tabs={tabs} screen={screen} onSelect={go} search={searchOn ? { open: searchOpen, onOpen: () => setSearchOpen(true) } : undefined} />}
      {!desktop && searchOn && <SearchSheet serverBase={serverBase} open={searchOpen} onClose={() => setSearchOpen(false)} onOpen={openResult} onAdd={onAdd} />}
      {addTo.picker}

      {killSwitchOpen && canReadOwner && (
        <KillSwitchDialog serverBase={serverBase} mode={s?.mode} onClose={() => setKillSwitchOpen(false)} onDone={() => void status.refresh()} />
      )}
      <SignInSheet />
    </div>
  );
}
