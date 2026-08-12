// gnomon — ISOLATED-world courier.
//
// Carries messages from page.js to the service worker. It does nothing else:
// no fetching, no parsing, no inspection of the payload.

(() => {
  'use strict';

  window.addEventListener('message', (event) => {
    try {
      // Only accept messages this page posted to itself.
      if (event.source !== window) {
        return;
      }
      if (event.origin !== window.location.origin) {
        return;
      }

      const data = event.data;
      if (!data || data.__gnomon !== 1) {
        return;
      }
      if (data.kind !== 'usage' || typeof data.raw !== 'string') {
        return;
      }

      // A sleeping or reloaded service worker makes this reject. That is
      // normal; swallow it so claude.ai's console stays clean.
      chrome.runtime.sendMessage({ kind: 'usage', raw: data.raw }, () => {
        void chrome.runtime.lastError;
      });
    } catch (_) {
      /* never surface to the page */
    }
  });
})();
