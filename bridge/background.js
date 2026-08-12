// gnomon — service worker.
//
// Hands each usage reading to the native host and records what happened in
// chrome.storage.local. Counters must be persisted: MV3 terminates this worker
// when idle, so in-memory counters silently reset to zero and read as "nothing
// ever arrived".
//
// NOTE ON FIDELITY: sendNativeMessage takes a JS object and Chrome serializes
// it, so the raw text must be parsed here. The bytes the native host receives
// are therefore Chrome's re-serialization, not the server's original text. See
// bridge/README.md.

'use strict';

const HOST = 'com.gnomon.bridge';
const STATS_KEY = 'gnomon_stats';

function emptyStats() {
  return {
    seen: 0,
    ok: 0,
    fail: 0,
    lastOkAt: null,
    lastFailAt: null,
    lastError: null,
  };
}

async function readStats() {
  const stored = await chrome.storage.local.get(STATS_KEY);
  return stored[STATS_KEY] || emptyStats();
}

// Serialization: every update is appended to this single promise chain, so the
// read-modify-write pairs run strictly one after another and two overlapping
// messages cannot both read the same value and each write back n+1.
let writeChain = Promise.resolve();

function updateStats(mutate) {
  writeChain = writeChain
    .then(async () => {
      const stats = await readStats();
      mutate(stats);
      await chrome.storage.local.set({ [STATS_KEY]: stats });
    })
    // Swallowed so one failed write cannot reject the chain and skip every
    // update queued behind it.
    .catch((e) => {
      console.warn('gnomon: could not persist stats:', e && e.message);
    });
  return writeChain;
}

function recordOk() {
  return updateStats((s) => {
    s.ok += 1;
    s.lastOkAt = Date.now();
  });
}

function recordFail(reason) {
  return updateStats((s) => {
    s.fail += 1;
    s.lastFailAt = Date.now();
    s.lastError = reason;
  });
}

// Exists because chrome.runtime.sendMessage does not deliver to the sending
// context, so the service worker cannot query itself via the 'stats' message.
globalThis.gnomonStats = async () => {
  const r = await chrome.storage.local.get('gnomon_stats');
  return r.gnomon_stats || null;
};

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || typeof msg !== 'object') {
    return false;
  }

  // Works from other extension contexts (popup, options page). Reading is async
  // now, so this branch must return true to keep sendResponse alive.
  if (msg.kind === 'stats') {
    readStats()
      .then((stats) => sendResponse(stats))
      .catch(() => sendResponse(null));
    return true;
  }

  if (msg.kind !== 'usage' || typeof msg.raw !== 'string') {
    return false;
  }

  // Bumped before parsing and before the native call, so `seen` answers "did
  // the content script deliver anything at all" independently of what the
  // native host did with it.
  updateStats((s) => {
    s.seen += 1;
  });

  let payload;
  try {
    payload = JSON.parse(msg.raw);
  } catch (e) {
    recordFail('payload was not JSON');
    console.warn('gnomon: payload was not JSON:', e && e.message);
    return false;
  }

  try {
    chrome.runtime.sendNativeMessage(HOST, payload, (response) => {
      if (chrome.runtime.lastError) {
        const message = chrome.runtime.lastError.message;
        recordFail(message);
        console.warn('gnomon: native host error:', message);
        return;
      }
      if (response && response.ok === false) {
        recordFail(response.error || 'bridge rejected payload');
        console.warn('gnomon: bridge rejected payload:', response.error);
        return;
      }
      recordOk();
    });
  } catch (e) {
    const message = (e && e.message) || 'sendNativeMessage threw';
    recordFail(message);
    console.warn('gnomon: could not reach native host:', message);
  }

  return false;
});
