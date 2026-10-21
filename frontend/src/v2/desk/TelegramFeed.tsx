import { useState } from 'react';
import { ChevronDown, Send } from 'lucide-react';
import { hhmm } from '../lib/format';
import { feedStatusChip } from '../lib/labels';
import type { FeedAnswer, FeedItem } from '../lib/types';
import { Chip, ToneChip } from '../ui/Chip';

// Figma "Telegram message": the message as the channel posted it, and what
// became of it — Bought, Waiting, Closed, or Ignored (not a call, skipped).

function TelegramMessage({ item }: { item: FeedItem }) {
  const chip = feedStatusChip(item.status);
  return (
    <article className="flex flex-col gap-2">
      <header className="flex items-center gap-2">
        <span className="num-small text-fg-muted">{hhmm(item.sent_at)}</span>
        <Chip icon={<Send size={11} />}>
          <span className="max-w-[160px] truncate">{item.channel}</span>
        </Chip>
        {item.reply_to != null && <span className="type-caption text-fg-muted">reply</span>}
        {item.edited && <span className="type-caption text-fg-muted">edited</span>}
      </header>
      <div className="whitespace-pre-wrap break-words rounded-8 bg-panel px-3 py-2.5 type-body text-fg">{item.text}</div>
      <div>
        <ToneChip tone={chip.tone}>{chip.label}</ToneChip>
      </div>
    </article>
  );
}

/** Today's messages from the monitored chats, newest first, by channel. */
export function TelegramFeed({ feed, error, showTitle = true }: { feed: FeedAnswer | null; error: string | null; showTitle?: boolean }) {
  const [chat, setChat] = useState<number | 'all'>('all');
  const channels = feed?.channels ?? [];
  // A channel that stops posting drops out of the list: show everything again.
  const filter = chat !== 'all' && channels.some((c) => c.chat_id === chat) ? chat : 'all';
  const items = (feed?.items ?? []).filter((i) => filter === 'all' || i.chat_id === filter);

  return (
    <section aria-label="Telegram calls" className="flex flex-col gap-4">
      <div className="flex items-center gap-3">
        {showTitle && <h2 className="type-title text-fg">Telegram calls</h2>}
        <label className={`relative ${showTitle ? 'ml-auto' : ''}`}>
          <span className="sr-only">Channel</span>
          <select
            value={filter === 'all' ? 'all' : String(filter)}
            onChange={(e) => setChat(e.target.value === 'all' ? 'all' : Number(e.target.value))}
            className="h-7 appearance-none rounded-6 border border-line bg-canvas pl-2.5 pr-7 type-label text-fg-muted outline-none hover:text-fg focus:border-line-strong"
          >
            <option value="all">All channels</option>
            {channels.map((c) => (
              <option key={c.chat_id} value={c.chat_id}>
                {c.title}
              </option>
            ))}
          </select>
          <ChevronDown size={14} className="pointer-events-none absolute top-1/2 right-2 -translate-y-1/2 text-fg-muted" />
        </label>
      </div>
      {error && !feed && <p className="type-caption text-loss">Couldn&apos;t load the feed: {error}</p>}
      {feed && items.length === 0 && <p className="type-body text-fg-muted">No messages yet today.</p>}
      {items.map((item) => (
        <TelegramMessage key={item.id} item={item} />
      ))}
    </section>
  );
}
