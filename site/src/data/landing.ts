// Landing-page comparison, computed at build time from the committed benchmark JSON so it can't
// drift from the performance page. The table is a fixed list of the commands a jj user runs all day,
// on the large repository; a missing row fails the build instead of showing a stale figure. Only rows
// where girt and stock jj do the same work qualify: `jj util gc` is excluded because girt's gc does
// not rewrite existing packs as `git gc` does.
import rows from '../../../docs/jj-performance.json';
import { REPOSITORIES } from './jj-commands';
import { GIRT_RESPONSIBLE_RATIO, provenance, spanOf, type Row } from './jj-performance';

const allRows = rows as unknown as Row[];

export interface Comparison {
  /** The command as a jj user types it. */
  command: string;
  girtMs: number | null;
  stockMs: number;
  /** girt ÷ stock jj; null when girt did not finish. */
  ratio: number | null;
}

// The variant of each everyday command closest to how it is used day to day.
const EVERYDAY: [op: string, variant: string, command: string][] = [
  ['status', 'small-edit', 'jj status'],
  ['log', 'default', 'jj log'],
  ['diff', 'wc', 'jj diff'],
  ['show', 'default', 'jj show'],
  ['new', 'default', 'jj new'],
  ['describe', 'default', 'jj describe'],
  ['commit', 'default', 'jj commit'],
  ['squash', 'default', 'jj squash'],
  ['rebase', '1', 'jj rebase'],
  ['bookmark', 'move', 'jj bookmark move'],
  ['undo', 'default', 'jj undo'],
  ['git fetch', '1', 'jj git fetch'],
  ['git push', 'update', 'jj git push'],
  ['git clone', 'local-full-checkout', 'jj git clone'],
];

export const comparisonRepo = REPOSITORIES.large.label;

export const comparisons: Comparison[] = EVERYDAY.map(([op, variant, command]) => {
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

export interface Gap {
  command: string;
  repo: string;
  /** girt ÷ stock jj; null when girt did not finish. */
  ratio: number | null;
  issue: number | null;
}

/**
 * The comparable cells, across every repository size, where girt makes a jj user wait longest
 * beyond stock jj. Cells girt did not finish come first, longest stock time first.
 */
export const biggestGaps: Gap[] = comparableRows
  .filter((row) => row.stock_ms != null && (row.girt_ms == null || row.girt_ms > row.stock_ms))
  .map((row) => ({ row, extra: row.girt_ms == null ? Infinity : row.girt_ms - row.stock_ms! }))
  .sort((a, b) => b.extra - a.extra || b.row.stock_ms! - a.row.stock_ms!)
  .slice(0, 3)
  .map(({ row }) => ({
    command: `jj ${row.op}`,
    repo: REPOSITORIES[row.size].label,
    ratio: row.girt_ms == null ? null : row.girt_ms / row.stock_ms!,
    issue: row.issues?.[0] ?? null,
  }));
