// gnomon — MAIN-world reader.
//
// Runs in claude.ai's own JS context so fetch carries the session cookie.
// Patches nothing, defines no globals, and every callback body is wrapped in
// try/catch: this file must never throw into claude.ai's code.
//
// The usage body is forwarded as RAW TEXT. It is never JSON.parse'd and
// re-stringified here, so what leaves this file is byte-identical to what the
// server sent.

(() => {
  'use strict';

  const ORIGIN = window.location.origin;

  // Hard floor between two fetches, however they were triggered.
  const MIN_INTERVAL_MS = 2000;
  // Heartbeat tick. One timer, not two.
  const HEARTBEAT_MS = 60000;
  // While hidden, only fetch if the last reading is older than this.
  const HIDDEN_MAX_AGE_MS = 300000;
  // Collapse a burst of completions into one trailing fetch.
  const COMPLETION_DEBOUNCE_MS = 1500;

  let orgId = null;
  let lastFetchMs = 0;
  let debounceTimer = null;

  async function discoverOrg() {
    const res = await fetch('/api/organizations', { credentials: 'include' });
    if (!res.ok) {
      throw new Error('organizations HTTP ' + res.status);
    }

    const body = await res.json();
    // The endpoint has returned both shapes; accept either.
    const first = Array.isArray(body) ? body[0] : body;
    if (!first) {
      throw new Error('no organization in response');
    }

    const id = first.uuid || first.id;
    if (!id) {
      throw new Error('organization has neither uuid nor id');
    }
    return id;
  }

  async function fetchUsage() {
    const now = Date.now();
    if (now - lastFetchMs < MIN_INTERVAL_MS) {
      return;
    }
    // Claim the slot before awaiting so concurrent triggers cannot both pass.
    lastFetchMs = now;

    if (orgId === null) {
      orgId = await discoverOrg();
    }

    const res = await fetch('/api/organizations/' + orgId + '/usage', {
      credentials: 'include',
    });

    // Stale or wrong org: forget it so the next attempt rediscovers.
    if (res.status === 401 || res.status === 404) {
      orgId = null;
      return;
    }
    if (!res.ok) {
      return;
    }

    // Raw text, deliberately. Do not parse and re-serialize.
    const raw = await res.text();
    window.postMessage({ __gnomon: 1, kind: 'usage', raw: raw }, ORIGIN);
  }

  // Every scheduling path funnels through here so nothing can throw outward.
  function trigger() {
    try {
      fetchUsage().catch(() => {});
    } catch (_) {
      /* never surface to the page */
    }
  }

  // (a) once at startup
  trigger();

  // (b) when the document becomes visible
  try {
    document.addEventListener('visibilitychange', () => {
      try {
        if (!document.hidden) {
          trigger();
        }
      } catch (_) {
        /* ignore */
      }
    });
  } catch (_) {
    /* ignore */
  }

  // (c) single 60s heartbeat; while hidden it only fires once the reading is
  // older than HIDDEN_MAX_AGE_MS.
  try {
    setInterval(() => {
      try {
        if (document.hidden && Date.now() - lastFetchMs < HIDDEN_MAX_AGE_MS) {
          return;
        }
        trigger();
      } catch (_) {
        /* ignore */
      }
    }, HEARTBEAT_MS);
  } catch (_) {
    /* ignore */
  }

  // (d) completions, debounced to a single trailing call
  try {
    const observer = new PerformanceObserver((list) => {
      try {
        const hit = list.getEntries().some(
          (entry) => entry.name && entry.name.indexOf('/completion') !== -1
        );
        if (!hit) {
          return;
        }
        if (debounceTimer !== null) {
          clearTimeout(debounceTimer);
        }
        debounceTimer = setTimeout(() => {
          debounceTimer = null;
          trigger();
        }, COMPLETION_DEBOUNCE_MS);
      } catch (_) {
        /* ignore */
      }
    });
    observer.observe({ type: 'resource', buffered: true });
  } catch (_) {
    /* ignore */
  }
})();
