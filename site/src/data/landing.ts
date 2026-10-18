// Landing-page comparison, computed at build time from the committed benchmark JSON so it can't
// drift from the performance page. The rows are a hand-picked mix of the best and worst cells on the
// large repository; a missing row fails the build instead of showing a stale figure. Only rows
// where girt and stock jj do the same work qualify: `jj util gc` is excluded because girt's gc does
// not rewrite existing packs as `git gc` does.
import rows from '../../../docs/jj-performance.json';
import { GIRT_RESPONSIBLE_RATIO, provenance, spanOf, type Row } from './jj-performance';

const allRows = rows as unknown as Row[];

export interface Comparison {
  /** The command as a jj user types it. */
  command: string;
  girtMs: number | null;
  stockMs: number;
  gitMs: number | null;
  /** girt ÷ stock jj; null when girt did not finish. */
  ratio: number | null;
}

const PICKS: [op: string, variant: string, command: string][] = [
  ['git fetch', '1', 'jj git fetch (one new commit)'],
  ['status', 'untracked', 'jj status (untracked files)'],
  ['log', 'default', 'jj log'],
  ['status', 'clean', 'jj status'],
  ['git fetch', 'noop', 'jj git fetch (nothing new)'],
  ['log', 'ancestry', 'jj log (ancestry query)'],
];

export const comparisonRepo = 'Linux v6.1';

export const comparisons: Comparison[] = PICKS.map(([op, variant, command]) => {
  const row = allRows.find((r) => r.size === 'large' && r.op === op && r.variant === variant);
  if (!row || row.stock_ms == null) {
    throw new Error(`benchmark row large ${op} ${variant} is missing or has no stock jj time`);
  }
  if (row.comparable === false) {
    throw new Error(`benchmark row large ${op} ${variant} is not like-for-like and can't be a landing pick`);
  }
  return {
    command,
    girtMs: row.girt_ms,
    stockMs: row.stock_ms,
    gitMs: row.git_ms,
    ratio: row.girt_ms == null ? null : row.girt_ms / row.stock_ms,
  };
});

// Rows where girt and stock jj do different work are timed but not compared.
const comparableRows = allRows.filter((row) => row.comparable !== false);
const compared = comparableRows.filter((row) => row.ratio_vs_stock != null);

/**
 * Across every repository size: how many compared cells are less than 20% slower than stock jj
 * (faster cells included), out of the cells where both finished.
 */
export const parity = {
  atMost20PercentSlower: compared.filter((row) => row.ratio_vs_stock! < GIRT_RESPONSIBLE_RATIO).length,
  compared: compared.length,
  unfinished: comparableRows.length - compared.length,
};

const span = spanOf(allRows);

export const measured = {
  machine: provenance.machine.split(';')[0],
  versions: span.versions,
  dates: span.dates ?? provenance.dates.replace(/\s*\(.*\)$/, ''),
};
