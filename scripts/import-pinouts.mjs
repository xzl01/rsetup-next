#!/usr/bin/env node

import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const sourceArgument = process.argv[2];
if (!sourceArgument) {
  throw new Error("usage: node scripts/import-pinouts.mjs /path/to/pin-out [output] [commit]");
}

const sourceRoot = resolve(sourceArgument);
const outputPath = resolve(process.argv[3] || "data/pinouts.json");
const sourceCommit = process.argv[4] || "37411182dc965a857539312bdfef9d0d6ac84a4e";
const { examplePinouts } = await import(pathToFileURL(resolve(sourceRoot, "src/data/index.js")));

const extraPatterns = {
  raspberryPi: ["Raspberry Pi"],
  radxaZero3: ["Radxa ZERO 3W", "Radxa ZERO 3E"],
  rockPiE: ["Radxa ROCK Pi E"],
  rockPiS: ["Radxa ROCK Pi S"],
};

function connector(id, name, pins) {
  return {
    id,
    name,
    pins: pins.map((pin) => ({
      number: pin.number,
      name: pin.name,
      defaultFunction: pin.name,
      type: pin.type,
      gpio: pin.gpio ?? null,
      voltage: pin.voltage ?? null,
      functions: pin.functions || [],
      description: pin.description || null,
    })),
  };
}

const profiles = Object.entries(examplePinouts)
  .filter(([id]) => id !== "arduino")
  .map(([id, profile]) => ({
    id,
    name: profile.name,
    description: profile.description || null,
    layout: profile.layout || "40-pin",
    patterns: [...new Set([profile.name, id, ...(extraPatterns[id] || [])])],
    connectors: profile.connectors
      ? profile.connectors.map((item, index) => connector(`connector-${index + 1}`, item.name, item.pins))
      : [connector("main", "40-Pin GPIO Header", profile.pins)],
  }));

const catalog = {
  schemaVersion: 2,
  source: {
    repository: "https://github.com/xzl01/pin-out",
    commit: sourceCommit,
    license: "GPL-3.0-or-later",
  },
  profiles,
};

await mkdir(resolve(outputPath, ".."), { recursive: true });
await writeFile(outputPath, `${JSON.stringify(catalog, null, 2)}\n`);
console.log(`Imported ${profiles.length} SBC pinout profiles into ${outputPath}`);
