/** Where the view-only page lives: `/view`, or `/view?n=<number>` to open
 * one number's trades at once (the link to share). */
export const VIEW_PATH = '/view';

export function isViewPath(): boolean {
  return /^\/(v2\/)?view\/?$/.test(window.location.pathname);
}
