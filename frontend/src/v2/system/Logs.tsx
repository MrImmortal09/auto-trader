import { Download } from 'lucide-react';
import { streamToken } from '../lib/api';
import { EngineLog } from './EngineLog';
import { downloadLog } from './logLines';
import { useEngineLog } from './useEngineLog';
import { Button } from '../ui/Button';

/**
 * The engine log on a screen of its own (admin only): every engine event,
 * newest first and live — orders and each order's timing, signals, errors.
 * The same log the System screen shows; the server only streams it to the
 * admin.
 */
export function Logs({ serverBase, desktop }: { serverBase: string; desktop: boolean }) {
  const log = useEngineLog(serverBase, streamToken());
  const download = (
    <Button size={desktop ? 'md' : 'sm'} icon={<Download size={14} />} onClick={() => downloadLog(log.lines)} disabled={log.lines.length === 0}>
      Download log
    </Button>
  );

  if (!desktop) {
    return (
      <div className="flex flex-col gap-3 p-4">
        <div className="flex items-center justify-between">
          <h2 className="type-title text-fg">Engine log</h2>
          {download}
        </div>
        <EngineLog lines={log.lines} connected={log.connected} compact />
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 p-6">
      <div className="flex items-center justify-between gap-4">
        <p className="type-caption text-fg-subtle">Every engine event, newest first and live: orders and how long each took (Timing), signals and errors.</p>
        {download}
      </div>
      <section className="flex min-h-0 flex-1 flex-col rounded-12 border border-line bg-panel p-4">
        <EngineLog lines={log.lines} connected={log.connected} compact={false} fill />
      </section>
    </div>
  );
}
