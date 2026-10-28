// The trading panel's height while open: what a drag may set, and what a
// saved height is clamped to on a smaller window.

export const DEFAULT_PANEL_HEIGHT = 320;
const MIN_HEIGHT = 180;
const maxHeight = () => Math.max(MIN_HEIGHT, Math.round(window.innerHeight * 0.6));

export const clampPanelHeight = (h: number) => Math.min(maxHeight(), Math.max(MIN_HEIGHT, Math.round(h)));
