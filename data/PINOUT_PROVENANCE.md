# Pinout data provenance

## Imported pin-out snapshot (20 profiles)

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
`/boot` replace Function1 only when their controller and mux mode match. When
an Overlay supplies physical pads in its `exclusive` metadata, only those pads
are mapped. Without that metadata, UART matching is limited to TX/RX; CTS/RTS
require explicit pad evidence.

Regenerate the snapshot from a local pin-out checkout with:

```sh
node scripts/import-pinouts.mjs /path/to/pin-out
```

## Radxa Dragon Q8B (official documentation)

`pinouts/dragon-q8b.json` is maintained separately from the generated snapshot
and embedded alongside it. Re-running the importer does not overwrite this
profile. Together the two sources provide 21 SBC profiles.

Source: [Radxa Dragon Q8B 40-pin GPIO documentation](https://docs.radxa.com/dragon/q8b/hardware-use/pin-gpio),
retrieved 2026-09-07. Attribution: © 2026 Radxa Computer (Shenzhen) Co.,Ltd.
The documentation's **CC-BY-4.0** license is retained; the complete
text is in `CC-BY-4.0.txt`.

Transformations and interpretation:

- The table's **Function0** column supplies `name` and `defaultFunction` for all
  40 physical pins; Function1 and later columns supply mux candidates. This is
  a different column convention from the imported pin-out snapshot.
- Power label `3V3` is normalized to `3.3V`. Other signal names are preserved
  verbatim, including `SPI*_SCKL`, `CCI_I2C_SCL0` / `CCI_I2C_SDA1`, and the
  `MIRA` / `MRIA` clock suffixes. These unusual spellings are not silently
  corrected or treated as evidence of different wiring.
- GPIO numbers identify SoC pads, not stable Linux GPIO-chip numbers. GPIO
  voltage is left unspecified because the source table does not establish it.
  The example's `/dev/gpiochip4` is not hard-coded or used to request a line.
- `UART18` and `HS-UART18` remain distinct candidates. Overlay resource metadata
  bounds assignments to the pads it declares; SPI4 CS0 does not also claim CS1–3.
  Regression fixtures use metadata from
  [radxa-overlays](https://github.com/radxa-pkg/radxa-overlays/tree/main/arch/arm64/boot/dts/qcom/overlays)
  for `sc8280xp-uart18`, `sc8280xp-spi4-spidev`, and `sc8280xp-i2c8`.

This is a board pinout baseline, **not a measurement of active pinmux**. The
native EFI/BLS backend reads Q8B's saved Overlay selection separately from the
U-Boot backend and uses it to resolve configured functions. Unread configuration
is explicitly marked; saved selection is not proof of the firmware-selected
or currently running mux. Physical validation after an explicitly confirmed
write and boot is still required before treating the map as evidence of the
current wiring mode. See the [Q8B test report](../docs/testing/q8b-2026-09-07/report.md).
