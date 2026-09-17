import React, { useCallback, useSyncExternalStore } from 'react';

let state = {};
let theme = { background: '#202124', foreground: '#e8eaed', muted: '#9aa0a6', border: '#45484d', accent: '#8ab4f8', kind: 'dark' };
const subscribers = new Set();
const subscribe = listener => { subscribers.add(listener); return () => subscribers.delete(listener); };
const notify = () => { for (const listener of subscribers) listener(); };

export function postMessage(message) {
  window.webkit.messageHandlers.zedCanvas.postMessage(JSON.stringify(message));
}

export function useHostTheme() {
  return useSyncExternalStore(subscribe, () => theme);
}

export function useCanvasState(key, initialValue) {
  const snapshot = useSyncExternalStore(subscribe, () => Object.hasOwn(state, key) ? state[key] : undefined);
  const value = snapshot === undefined ? initialValue : snapshot;
  const setValue = useCallback(action => {
    const previous = Object.hasOwn(state, key) ? state[key] : initialValue;
    const next = typeof action === 'function' ? action(previous) : action;
    const serialized = JSON.stringify(next);
    if (serialized === undefined || serialized.length > 1024 * 1024) throw new Error('Canvas state must be a JSON value smaller than 1 MiB');
    state = { ...state, [key]: JSON.parse(serialized) };
    notify();
    postMessage({ kind: 'state', key, value: state[key] });
  }, [key, initialValue]);
  return [value, setValue];
}

export function useCanvasAction() {
  return useCallback(action => postMessage({ kind: 'action', action }), []);
}

export function updateState(nextState) {
  state = nextState;
  notify();
}

export function initializeHost(initialState, initialTheme) {
  state = initialState;
  updateTheme(initialTheme);
}

export function updateTheme(nextTheme) {
  theme = {
    ...nextTheme,
    text: { primary: nextTheme.foreground, secondary: nextTheme.muted, tertiary: nextTheme.muted, link: nextTheme.accent },
    bg: { editor: nextTheme.background, elevated: nextTheme.background },
    stroke: { primary: nextTheme.border, secondary: nextTheme.border, focused: nextTheme.accent },
    accent: { primary: nextTheme.accent },
    status: { success: 'hsl(145, 52%, 43%)', warning: 'hsl(38, 75%, 47%)', danger: 'hsl(5, 68%, 57%)', info: nextTheme.accent },
    category: [nextTheme.accent, 'hsl(153, 43%, 48%)', 'hsl(32, 72%, 54%)', 'hsl(280, 42%, 62%)', 'hsl(190, 56%, 45%)']
  };
  for (const [key, value] of Object.entries(nextTheme)) document.documentElement.style.setProperty(`--canvas-${key}`, value);
  document.documentElement.style.colorScheme = theme.kind;
  notify();
}

export class ErrorBoundary extends React.Component {
  state = { error: null };
  static getDerivedStateFromError(error) { return { error: String(error) }; }
  componentDidCatch(error, info) { postMessage({ kind: 'error', error: `${error}\n${info.componentStack}` }); }
  render() { return this.state.error ? <pre role="alert">{this.state.error}</pre> : this.props.children; }
}
