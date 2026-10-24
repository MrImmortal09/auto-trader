/** Crosshairs moving together across the charts on screen, by time. */
export class CrosshairBus {
  private subs = new Map<string, (t: number | null) => void>();
  subscribe(id: string, fn: (t: number | null) => void) {
    this.subs.set(id, fn);
    return () => {
      this.subs.delete(id);
    };
  }
  publish(from: string, t: number | null) {
    for (const [id, fn] of this.subs) if (id !== from) fn(t);
  }
}
