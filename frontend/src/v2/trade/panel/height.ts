// The trading panel's height while open. The Trade screen doesn't scroll:
// the panel grows up into the charts, which keep at least
// `MIN_CHART_HEIGHT`, and never past the bottom of the window.

export const DEFAULT_PANEL_HEIGHT = 320;
export const MIN_PANEL_HEIGHT = 180;
/** The least height the charts keep when the panel grows. */
export const MIN_CHART_HEIGHT = 200;

/** A saved height, before the screen is laid out: within the panel's least
 * and what a window this tall could hold. The panel measures itself to fit
 * once it is on screen. */
export const clampPanelHeight = (h: number) => Math.max(MIN_PANEL_HEIGHT, Math.min(Math.round(h), window.innerHeight - 300));
