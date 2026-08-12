# gnomon Chrome extension

Reads claude.ai usage from inside the page and forwards it to the gnomon native
messaging host. Requires `nativeMessaging` and `storage`, nothing else.

## Install

1. Build the host binary:

       cargo build --release -p gnomon-bridge

2. Open `chrome://extensions`, turn on **Developer mode**, click **Load
   unpacked**, and select this directory (`bridge/`).

3. Copy the extension ID Chrome shows on the new card (32 letters, a–p).

4. Register the native messaging host with that ID:

       ./target/release/gnomon-bridge --install-manifest <EXTENSION_ID>

   This writes `~/.config/google-chrome/NativeMessagingHosts/com.gnomon.bridge.json`
   and prints what it wrote.

5. Back on `chrome://extensions`, click **Reload** on the gnomon card. Chrome
   only reads the host manifest at worker startup.

6. Verify: open a claude.ai tab, then click **service worker** on the extension
   card and run `await gnomonStats()` in that console.

## Reading the stats

`seen` counts usage messages received from the content script; `ok` and `fail`
count native host outcomes; `lastOkAt` / `lastFailAt` are epoch milliseconds or
null; `lastError` is the last failure message or null. Counters live in
`chrome.storage.local`, so they survive the service worker being suspended.

- `seen` at 0 — the content script never delivered. Check the claude.ai tab
  console for `gnomon: forwarded N bytes`.
- `seen` rising, `ok` flat — delivery works; the native host is the problem.
  Read `lastError`.
- `ok` rising — readings are reaching the host.

## The extension ID is not stable

Chrome derives the ID from this directory's absolute path. Move or rename it and
the ID changes, the host manifest lists the wrong origin, and Chrome refuses the
connection. Re-run step 4 with the new ID and reload.

## Fidelity note

`sendNativeMessage` accepts an object, so `background.js` parses the raw text and
Chrome re-serializes it. The host receives Chrome's encoding: semantically
identical, not byte-identical.
