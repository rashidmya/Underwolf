// Diagnostic application, not Chromium native keyboard/mouse injection.
window.underwolfGeneration = 0;
window.underwolfPaused = false;
window.underwolfPending = 0;
const waiting = new Set();
const model = window.underwolfModel = { text: '', keys: new Set(), buttons: new Set(), wheel: [0, 0], pointer: [0, 0], focus: false };
window.underwolfSnapshot = () => ({ text: model.text, buttons: [...model.buttons], wheel: [...model.wheel], pointer: [...model.pointer] });
window.underwolfResume = () => { window.underwolfPaused = false; for (const r of waiting) r(); waiting.clear(); };
function draw() { const node = document.getElementById('diagnostics'); if (node) node.textContent = JSON.stringify({ ...window.underwolfSnapshot(), keys: [...model.keys], focus: model.focus }, null, 2); }
window.underwolfConsume = async (event, generation) => {
  if (generation !== window.underwolfGeneration) return false;
  if (event.type === 'record' && window.underwolfPaused) {
    window.underwolfPending++; await new Promise(r => waiting.add(r)); window.underwolfPending--;
  }
  if (generation !== window.underwolfGeneration) return false;
  if (event.type === 'reset') { model.keys.clear(); model.buttons.clear(); model.focus = false; model.text = ''; model.wheel = [0, 0]; model.pointer = [0, 0]; }
  if (event.type === 'state') { const node = document.getElementById('state'); if (node) node.textContent = `${event.state.exclusive ? 'Exclusive' : 'Standard'} · epoch ${event.state.epoch}`; }
  if (event.type === 'table') { const node = document.getElementById('policy'); if (node) node.textContent = event.config.widgets.map(w => `${w.instance.id}:${w.instance.generation} · ${w.mode}`).join(', '); }
  if (event.type === 'record') {
    const r = event.record, v = r.input;
    if (r.kind === 'cancel') { model.keys.clear(); model.buttons.clear(); model.focus = false; }
    if (r.kind === 'focus') model.focus = r.argument === 1;
    if (r.kind === 'input') {
      if (v.kind === 1) { if (v.flags & 1) model.keys.add(v.a); else model.keys.delete(v.a); }
      if (v.kind === 2) model.text = (model.text + String.fromCodePoint(v.a)).slice(-200);
      if (v.kind === 3) model.pointer = [event.local.x, event.local.y];
      if (v.kind === 4) { if (v.flags & 1) model.buttons.add(v.a); else model.buttons.delete(v.a); }
      if (v.kind === 5) { model.wheel[0] += v.a; model.wheel[1] += v.b; }
    }
  }
  draw(); return true;
};
