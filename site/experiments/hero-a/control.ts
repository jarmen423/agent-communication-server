// The visitor's pause switch (WCAG 2.2.2): shared by the button in the stage and
// the lazily loaded scene module, without either importing the other.
type Listener = (paused: boolean) => void;

let paused = false;
const listeners = new Set<Listener>();

export const motion = {
  get paused() {
    return paused;
  },
  set(next: boolean) {
    if (next === paused) return;
    paused = next;
    listeners.forEach((l) => l(paused));
  },
  subscribe(l: Listener) {
    listeners.add(l);
    return () => void listeners.delete(l);
  },
};
