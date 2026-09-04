# Pinout data provenance

`pinouts.json` is a normalized snapshot of the SBC data from
[`xzl01/pin-out`](https://github.com/xzl01/pin-out), commit
`37411182dc965a857539312bdfef9d0d6ac84a4e`.

Copyright (c) 2025-2026 xzl01.

The copyright owner has authorized this transformed snapshot to be distributed
with rsetup-next under `GPL-3.0-or-later`. The import removes presentation-only
fields and preserves board, connector, physical-pin, voltage, GPIO and mux
metadata. The source `name` field is normalized as `defaultFunction`: it is the
Function1 column used when no Overlay changes that physical pin. Function2 and
later mux entries remain Overlay candidates. Saved enabled Overlay files under
`/boot` replace Function1 only when their controller and mux mode match.

Regenerate the snapshot from a local pin-out checkout with:

```sh
node scripts/import-pinouts.mjs /path/to/pin-out
```
