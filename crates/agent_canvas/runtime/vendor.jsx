import React from 'react';
import * as ReactExports from 'react';
import * as ReactDOM from 'react-dom/client';
import * as JSX from 'react/jsx-runtime';

import * as SDK from './sdk.jsx';
import * as Host from './host.jsx';

globalThis.__zedCanvasModules = {
  react: { ...ReactExports, default: React },
  'react/jsx-runtime': JSX,
  '@zed/canvas': SDK,
};

let selecting = false;
let hoveredElement;
let pickedElements = [];
globalThis.__zedCanvasHost = {
  updateTheme: Host.updateTheme,
  updateState: Host.updateState,
  setSelecting(value) {
    selecting = value;
    if (hoveredElement?.getAttribute('data-zed-selected') === 'hover') hoveredElement.removeAttribute('data-zed-selected');
    hoveredElement = undefined;
    if (value) globalThis.__zedCanvasHost.clearSelection();
    document.body.classList.toggle('zed-selecting', value);
  },
  clearSelection() {
    for (const element of pickedElements) element.removeAttribute('data-zed-selected');
    pickedElements = [];
  },
  mount(state, theme) {
    Host.initializeHost(state, theme);
    function Ready() {
      React.useEffect(() => { Host.postMessage({ kind: 'ready' }); }, []);
      return <CanvasModule.default />;
    }
    ReactDOM.createRoot(document.getElementById('root')).render(<Host.ErrorBoundary><Ready /></Host.ErrorBoundary>);
  },
};

window.addEventListener('error', event => Host.postMessage({ kind: 'error', error: `${event.message}\n${event.filename}:${event.lineno}:${event.colno}` }));
window.addEventListener('unhandledrejection', event => Host.postMessage({ kind: 'error', error: String(event.reason) }));
document.addEventListener('mouseover', event => {
  if (!selecting || !(event.target instanceof Element)) return;
  if (hoveredElement?.getAttribute('data-zed-selected') === 'hover') hoveredElement.removeAttribute('data-zed-selected');
  hoveredElement = event.target;
  if (!pickedElements.includes(hoveredElement)) hoveredElement.setAttribute('data-zed-selected', 'hover');
}, true);
document.addEventListener('click', event => {
  if (selecting && event.target instanceof Element) {
    event.preventDefault();
    event.stopImmediatePropagation();
    const element = event.target;
    if (pickedElements.includes(element) && event.shiftKey) {
      pickedElements = pickedElements.filter(item => item !== element);
      element.removeAttribute('data-zed-selected');
    } else if (!pickedElements.includes(element) && pickedElements.length < 16) {
      pickedElements.push(element);
      element.setAttribute('data-zed-selected', 'picked');
    }
    Host.postMessage({ kind: 'selection', complete: !event.shiftKey, elements: pickedElements.map(element => {
      const rectangle = element.getBoundingClientRect();
      return {
        id: element.closest('[data-canvas-id]')?.getAttribute('data-canvas-id') ?? null,
        tag: element.tagName.toLowerCase(), text: element.textContent.slice(0, 2000), html: element.outerHTML.slice(0, 4000),
        bounds: { x: rectangle.x, y: rectangle.y, width: rectangle.width, height: rectangle.height },
      };
    }) });
    if (!event.shiftKey) globalThis.__zedCanvasHost.setSelecting(false);
    return;
  }
  const link = event.target instanceof Element ? event.target.closest('a[href]') : null;
  if (link && !link.getAttribute('href').startsWith('#')) {
    event.preventDefault();
    Host.postMessage({ kind: 'link', url: link.href });
  }
}, true);

document.addEventListener('keydown', event => {
  if (event.isTrusted && event.metaKey && event.shiftKey && event.key.toLowerCase() === 'p') {
    event.preventDefault();
    Host.postMessage({ kind: 'shortcut', action: 'command_palette' });
  }
}, true);
