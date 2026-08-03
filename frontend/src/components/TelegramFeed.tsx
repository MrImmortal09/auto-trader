import { useEffect, useState } from 'react';
import { MessageCircle } from 'lucide-react';
import { apiFetch } from '../lib/api';

export function TelegramFeed({ serverBase }: { serverBase: string }) {
  const [messages, setMessages] = useState<{ id: string, time: string, text: string, chatId: number, isEdit: boolean }[]>([]);

  useEffect(() => {
    function load() {
      apiFetch(serverBase, '/api/logs/history?lines=1000')
        .then(r => r.json())
        .then((history: { id: number; time: string; text: string; is_error: boolean }[]) => {
           const tgMsgs: any[] = [];
           for (const log of history) {
             try {
               const p = JSON.parse(log.text);
               if (p.event === 'TELEGRAM_MESSAGE' || p.event === 'TELEGRAM_MESSAGE_EDITED') {
                 tgMsgs.push({
                   id: log.id.toString(),
                   time: log.time,
                   text: p.text,
                   chatId: p.chat_id,
                   isEdit: p.event === 'TELEGRAM_MESSAGE_EDITED'
                 });
               }
             } catch { /* ignore */ }
           }
           setMessages(tgMsgs.reverse()); // latest first
        })
        .catch(console.error);
    }
    load();

    const apiUrlLocal = (base: string, path: string) => {
      if (base) return `${base}${path}`;
      return `http://${window.location.hostname}:8080${path}`;
    };

    let es: EventSource | null = null;
    try {
      es = new EventSource(apiUrlLocal(serverBase, '/api/logs/stream'));
      es.onmessage = (event) => {
        try {
          const log = JSON.parse(event.data);
          const p = JSON.parse(log.text);
          if (p.event === 'TELEGRAM_MESSAGE' || p.event === 'TELEGRAM_MESSAGE_EDITED') {
            setMessages(prev => {
              const newMsg = {
                id: log.id?.toString() || Date.now().toString(),
                time: log.time || new Date().toISOString(),
                text: p.text,
                chatId: p.chat_id,
                isEdit: p.event === 'TELEGRAM_MESSAGE_EDITED'
              };
              // Check if we already have this msg (in case it came from history right before SSE)
              if (prev.some(m => m.id === newMsg.id)) return prev;
              return [newMsg, ...prev].slice(0, 100);
            });
          }
        } catch { /* ignore */ }
      };
    } catch (e) {
      console.error('SSE Error:', e);
    }

    return () => {
      if (es) es.close();
    };
  }, [serverBase]);

  if (messages.length === 0) return null;

  return (
    <div className="bg-surface-container-lowest border-b border-outline-variant shrink-0">
      <div className="px-4 py-2 border-b border-outline-variant text-label-caps text-on-surface-variant uppercase tracking-wide bg-surface-container-low relative z-10 flex items-center gap-2 font-semibold">
        <MessageCircle size={14} className="text-primary" />
        Recent Telegram Signals
      </div>
      <div className="max-h-64 overflow-y-auto p-4 flex flex-col gap-3">
        {messages.map(msg => (
          <div key={msg.id} className="bg-surface-container-lowest border border-outline-variant rounded-lg p-3 text-sm flex flex-col gap-1 shadow-sm">
            <div className="flex justify-between items-center text-xs text-on-surface-variant">
              <span className="text-primary font-semibold">Chat ID: {msg.chatId}</span>
              <span>{msg.time} {msg.isEdit && '(Edited)'}</span>
            </div>
            <div className="text-on-surface whitespace-pre-wrap font-mono-code text-xs">{msg.text}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
