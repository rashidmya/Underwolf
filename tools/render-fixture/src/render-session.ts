// Native session state can change between an event and a synchronous call.
export function renderCall(operation: () => void, disconnected: (reason: string) => void): boolean {
  try { operation(); return true; }
  catch (error) {
    if (!(error instanceof Error) || error.message !== 'UW_RENDER_Stale') throw error;
    disconnected('render session changed before the synchronous call'); return false;
  }
}
