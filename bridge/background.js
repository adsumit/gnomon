// gnomon — service worker.
//
// Hands each usage reading to the native host. Counters are in memory only and
// reset whenever the worker is recycled, which Chrome does aggressively.
//
// NOTE ON FIDELITY: sendNativeMessage takes a JS object and Chrome serializes
// it, so the raw text must be parsed here. The bytes the native host receives
// are therefore Chrome's re-serialization, not the server's original text. See
// bridge/README.md.

'use strict';

const HOST = 'com.gnomon.bridge';

let okCount = 0;
let failCount = 0;

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || typeof msg !== 'object') {
    return false;
  }

  // Queryable from the service worker console:
  //   chrome.runtime.sendMessage({ kind: 'stats' }, console.log)
  if (msg.kind === 'stats') {
    sendResponse({ ok: okCount, fail: failCount });
    return false;
  }

  if (msg.kind !== 'usage' || typeof msg.raw !== 'string') {
    return false;
  }

  let payload;
  try {
    payload = JSON.parse(msg.raw);
  } catch (e) {
    failCount += 1;
    console.warn('gnomon: payload was not JSON:', e && e.message);
    return false;
  }

  try {
    chrome.runtime.sendNativeMessage(HOST, payload, (response) => {
      if (chrome.runtime.lastError) {
        failCount += 1;
        console.warn('gnomon: native host error:', chrome.runtime.lastError.message);
        return;
      }
      if (response && response.ok === false) {
        failCount += 1;
        console.warn('gnomon: bridge rejected payload:', response.error);
        return;
      }
      okCount += 1;
    });
  } catch (e) {
    failCount += 1;
    console.warn('gnomon: could not reach native host:', e && e.message);
  }

  return false;
});
