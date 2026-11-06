// Undo/redo history for chart drawings: a stack of snapshots per instrument,
// capped at 50. A new change after an undo clears the redo stack.

const MAX = 50;

/** A capped undo/redo stack of `T` snapshots, keyed by instrument symbol. */
export class DrawHistory<T> {
  private undos = new Map<string, T[]>();
  private redos = new Map<string, T[]>();

  /** Push the *previous* state before a change. */
  push(symbol: string, snapshot: T) {
    const stack = this.undos.get(symbol) ?? [];
    stack.push(snapshot);
    if (stack.length > MAX) stack.shift();
    this.undos.set(symbol, stack);
    // A new change after an undo clears the redo stack.
    this.redos.delete(symbol);
  }

  /** Pop the last state; the caller must pass the *current* state for redo. */
  undo(symbol: string, current: T): T | undefined {
    const stack = this.undos.get(symbol);
    const prev = stack?.pop();
    if (prev === undefined) return undefined;
    const redo = this.redos.get(symbol) ?? [];
    redo.push(current);
    if (redo.length > MAX) redo.shift();
    this.redos.set(symbol, redo);
    return prev;
  }

  /** Pop the last redo state; the caller must pass the *current* state for undo. */
  redo(symbol: string, current: T): T | undefined {
    const stack = this.redos.get(symbol);
    const next = stack?.pop();
    if (next === undefined) return undefined;
    const undo = this.undos.get(symbol) ?? [];
    undo.push(current);
    if (undo.length > MAX) undo.shift();
    this.undos.set(symbol, undo);
    return next;
  }

  canUndo(symbol: string): boolean {
    return (this.undos.get(symbol)?.length ?? 0) > 0;
  }

  canRedo(symbol: string): boolean {
    return (this.redos.get(symbol)?.length ?? 0) > 0;
  }
}
