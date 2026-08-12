# gnomon Chrome extension

Reads claude.ai usage from inside the page and forwards it to the gnomon native
messaging host. Requires `nativeMessaging` and nothing else — no host
permissions, no network access of its own.

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
   card and run in that console:

       chrome.runtime.sendMessage({ kind: 'stats' }, console.log)

   A rising `ok` count means readings are reaching the host.

## The extension ID is not stable

Chrome derives the ID from the unpacked directory's absolute path. Move or
rename this directory, or load it from a different path, and the ID changes —
the old host manifest then lists the wrong origin and Chrome refuses the
connection. Re-run step 4 with the new ID and reload.

## Fidelity note

`sendNativeMessage` accepts an object, so `background.js` parses the raw text
and Chrome re-serializes it. The host receives Chrome's encoding, not the
server's original bytes. The document is semantically identical; the exact
byte sequence is not preserved on this hop.
