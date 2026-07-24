// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kirk D. Brown

// Result-row schema and the merge that writes results.*.json.
//
// Every row carries the platform it was measured on and the memory metric that
// produced its `rss` field. That matters because the merge key includes the
// platform: a macOS run adds rows beside the Linux ones instead of replacing
// them, so both platforms' numbers coexist in one file and neither is silently
// clobbered by a run on the other host.
//
// Rows written before platform tagging existed have no `platform` field. They
// were all measured on Linux, so they are read as such (see rowPlatform) — that
// keeps historical data meaningful without a migration.
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { PLATFORM, MEM_METRIC } from "./host-mem.mjs";

/** Platform a row was measured on; untagged legacy rows are Linux. */
export const rowPlatform = (r) => r.platform ?? "linux";

/** Memory metric a row's `rss` is expressed in; legacy rows are Linux PSS. */
export const rowMemMetric = (r) => r.mem_metric ?? (rowPlatform(r) === "linux" ? "pss" : null);

/** Stamp the current host onto freshly measured rows. */
export function tagRows(rows) {
  return rows.map((r) => ({
    ...r,
    platform: PLATFORM,
    ...(r.rss === undefined || r.rss === null ? {} : { mem_metric: MEM_METRIC }),
  }));
}

/**
 * Merge freshly measured rows into an existing results file.
 *
 * The key includes the platform, so a run only replaces rows measured on the
 * SAME host platform. `fresh` is expected to be tagged already (tagRows).
 */
export async function mergeRows(here, file, fresh) {
  let existing = [];
  try {
    existing = JSON.parse(await readFile(join(here, file), "utf8"));
  } catch {}
  const key = (r) => `${r.browser}/${r.impl}/${r.wl}/${rowPlatform(r)}`;
  const produced = new Set(fresh.map(key));
  return [...existing.filter((r) => !produced.has(key(r))), ...fresh];
}

/** Rows measured on one platform (default: the current host). */
export const forPlatform = (rows, platform = PLATFORM) =>
  rows.filter((r) => rowPlatform(r) === platform);

/** Distinct platforms present in a row set, Linux first for stable report order. */
export function platformsIn(rows) {
  const seen = [...new Set(rows.map(rowPlatform))];
  return seen.sort((a, b) => (a === "linux" ? -1 : b === "linux" ? 1 : a.localeCompare(b)));
}
