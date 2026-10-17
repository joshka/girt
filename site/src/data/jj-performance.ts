// Build-time view of the committed jj benchmark matrix. The numbers come only from the JSON files
// in the repository's docs/ directory; this module classifies and formats them.
import rows from '../../../docs/jj-performance.json';
import provenanceJson from '../../../docs/jj-performance-provenance.json';

export type Size = 'small' | 'medium' | 'large';
export const SIZES: Size[] = ['small', 'medium', 'large'];

export interface Row {
  repo: string;
  size: Size;
  op: string;
  variant: string;
  status: string;
  tool_status?: Record<string, string>;
  girt_ms: number | null;
  stock_ms: number | null;
  git_ms: number | null;
  girt_samples: number;
  ratio_vs_stock: number | null;
  jj_level_gap: boolean;
  issues?: number[];
  notes: string;
  timeout_limits_s?: Record<string, number>;
}

export interface Fixture {
  name: string;
  head: string;
  objects: string;
  refs: string;
  tags: number;
  worktree_files: number;
  pack_bytes: number;
}

export interface Provenance {
  dates: string;
  machine: string;
  girt_commit: string;
  jj_adapter_commit: string;
  jj_stock_base: string;
  git_version: string;
  git_commit: string;
  build: string;
  method: string;
  fixtures: Record<Size, Fixture>;
  current_main_rebuild?: { girt_commit: string };
}

export const provenance = provenanceJson as unknown as Provenance;
const allRows = rows as unknown as Row[];

/** How often a jj user runs an operation; each frequency has its own notion of "slow". */
export type Frequency = 'constant' | 'hourly' | 'occasional';

export interface FrequencyGroup {
  id: Frequency;
  title: string;
  description: string;
  /** Upper bounds, in milliseconds, of the first three bands; anything above is the fourth. */
  bounds: [number, number, number];
}

export const GROUPS: FrequencyGroup[] = [
  {
    id: 'constant',
    title: 'Constantly',
    description: 'Commands run many times an hour while working: inspecting and editing changes.',
    bounds: [100, 300, 1_000],
  },
  {
    id: 'hourly',
    title: 'Hourly',
    description: 'Commands run several times a day: syncing with remotes and reshaping history.',
    bounds: [1_000, 3_000, 10_000],
  },
  {
    id: 'occasional',
    title: 'Occasionally',
    description: 'Setup, maintenance and full-history dumps.',
    bounds: [10_000, 60_000, 300_000],
  },
];

// Keyed by `op` or `op/variant`; the more specific key wins. Unclassified rows fail the build so a
// new benchmark operation cannot silently drop off the page.
const FREQUENCY: Record<string, Frequency> = {
  status: 'constant',
  log: 'constant',
  'log/large': 'occasional',
  'log/ancestry': 'occasional',
  show: 'constant',
  'op log': 'constant',
  describe: 'constant',
  new: 'constant',
  commit: 'constant',
  bookmark: 'constant',
  diff: 'constant',
  'diff/large': 'hourly',
  edit: 'constant',
  evolog: 'constant',
  abandon: 'constant',
  undo: 'constant',
  restore: 'constant',
  'file show': 'constant',
  'git fetch': 'hourly',
  'git push': 'hourly',
  rebase: 'hourly',
  squash: 'hourly',
  split: 'hourly',
  duplicate: 'hourly',
  'file annotate': 'hourly',
  'op restore': 'hourly',
  'git import': 'hourly',
  'git export': 'hourly',
  'git clone': 'occasional',
  'git init': 'occasional',
  'util gc': 'occasional',
  'workspace add': 'occasional',
};

export function frequencyOf(op: string, variant: string): Frequency {
  const frequency = FREQUENCY[`${op}/${variant}`] ?? FREQUENCY[op];
  if (!frequency) {
    throw new Error(`jj benchmark operation "${op} ${variant}" has no frequency group`);
  }
  return frequency;
}

/** Band 0 is the fastest; band 3 is beyond the group's last bound. */
export function bandOf(ms: number, bounds: FrequencyGroup['bounds']): number {
  const index = bounds.findIndex((bound) => ms <= bound);
  return index === -1 ? bounds.length : index;
}

/** girt is responsible for a slow cell when it is at least this much slower than stock jj. */
export const GIRT_RESPONSIBLE_RATIO = 1.2;

export type Outcome = 'ok' | 'refused' | 'timeout' | 'missing';

export interface Cell {
  row: Row;
  outcome: Outcome;
  girtBand: number | null;
  stockBand: number | null;
  /** Reasons a slow cell is slow: girt (≥20% over stock jj) and/or jj (stock jj is slow too). */
  blame: ('girt' | 'jj' | 'boundary')[];
  /** Workload details specific to this repository size; shared details are on the operation. */
  workload: string[];
}

function outcomeOf(row: Row): Outcome {
  if (row.status === 'ok' && row.girt_ms != null) return 'ok';
  if (row.status === 'timeout') return 'timeout';
  if (row.status === 'unsupported' || row.tool_status?.girt === 'refusal') return 'refused';
  return 'missing';
}

function cellOf(row: Row, group: FrequencyGroup): Cell {
  const outcome = outcomeOf(row);
  const girtBand = outcome === 'ok' ? bandOf(row.girt_ms!, group.bounds) : null;
  const stockBand = row.stock_ms == null ? null : bandOf(row.stock_ms, group.bounds);
  const blame: Cell['blame'] = [];
  if (girtBand != null && girtBand > 0) {
    const ratio = row.ratio_vs_stock ?? 0;
    if (ratio >= GIRT_RESPONSIBLE_RATIO) blame.push('girt');
    if (stockBand != null && stockBand > 0) blame.push('jj');
    // Neither: stock jj is just inside a bound and girt is less than 20% slower.
    if (blame.length === 0) blame.push('boundary');
  }
  return { row, outcome, girtBand, stockBand, blame, workload: workloadOf(row.notes) };
}

export interface Operation {
  key: string;
  op: string;
  variant: string;
  /** Workload details shared by every measured size. */
  workload: string;
  cells: Partial<Record<Size, Cell>>;
}

/** Workload description parts, without the per-tool sample ranges or failure details. */
function workloadOf(notes: string): string[] {
  return notes
    .split(';')
    .map((part) => part.trim())
    .filter(
      (part) =>
        part &&
        !/^(girt|stock|git) n=/.test(part) &&
        !/^girt unsupported:/.test(part) &&
        !/^(failed attempts|no successful timing|timeout:)/.test(part),
    );
}

export function operationsFor(group: FrequencyGroup): Operation[] {
  const byKey = new Map<string, Operation>();
  for (const row of allRows) {
    if (frequencyOf(row.op, row.variant) !== group.id) continue;
    const key = `${row.op} ${row.variant}`;
    let operation = byKey.get(key);
    if (!operation) {
      operation = { key, op: row.op, variant: row.variant, workload: '', cells: {} };
      byKey.set(key, operation);
    }
    operation.cells[row.size] = cellOf(row, group);
  }
  // Workloads can differ by size (a different commit or file count). Show the parts every size
  // shares once on the row, and leave each cell only its own details.
  for (const operation of byKey.values()) {
    const cells = Object.values(operation.cells);
    const shared = cells[0].workload.filter((part) =>
      cells.every((cell) => cell.workload.includes(part)),
    );
    operation.workload = shared.join('; ');
    for (const cell of cells) {
      cell.workload = cell.workload.filter((part) => !shared.includes(part));
    }
  }
  return [...byKey.values()].sort((a, b) => a.key.localeCompare(b.key));
}

export interface PainPoint {
  group: FrequencyGroup;
  operation: Operation;
  size: Size;
  cell: Cell;
  /** Extra wall-clock time girt adds over stock jj, or null for failures. */
  extraMs: number | null;
}

/**
 * Cells a user would notice: girt fails or times out, or girt is at least 20% slower than stock jj
 * and either moves the operation into a slower band or adds at least the group's first bound (for
 * example 100 ms for a constantly run command). Failures come first, then more frequent operations,
 * then the largest added time.
 */
export function painPoints(): PainPoint[] {
  const points: PainPoint[] = [];
  for (const group of GROUPS) {
    for (const operation of operationsFor(group)) {
      for (const size of SIZES) {
        const cell = operation.cells[size];
        if (!cell) continue;
        const { row } = cell;
        if (cell.outcome !== 'ok') {
          points.push({ group, operation, size, cell, extraMs: null });
        } else if (cell.blame.includes('girt') && cell.stockBand != null) {
          const extraMs = row.girt_ms! - row.stock_ms!;
          if (cell.girtBand! > cell.stockBand || extraMs >= group.bounds[0]) {
            points.push({ group, operation, size, cell, extraMs });
          }
        }
      }
    }
  }
  const groupOrder = (point: PainPoint) => GROUPS.indexOf(point.group);
  return points.sort(
    (a, b) =>
      Number(a.extraMs != null) - Number(b.extraMs != null) ||
      groupOrder(a) - groupOrder(b) ||
      (b.extraMs ?? 0) - (a.extraMs ?? 0),
  );
}

/** Slow cells where stock jj is already in a slow band, so girt alone cannot fix them. */
export function jjLevelSlowCells(): PainPoint[] {
  const points: PainPoint[] = [];
  for (const group of GROUPS) {
    for (const operation of operationsFor(group)) {
      for (const size of SIZES) {
        const cell = operation.cells[size];
        if (cell?.stockBand != null && cell.stockBand >= 2) {
          points.push({ group, operation, size, cell, extraMs: null });
        }
      }
    }
  }
  return points;
}

export function formatMs(ms: number): string {
  if (ms < 1_000) return `${Math.round(ms)} ms`;
  if (ms < 10_000) return `${(ms / 1_000).toFixed(2)} s`;
  if (ms < 60_000) return `${(ms / 1_000).toFixed(1)} s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1_000);
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`;
}

export function formatBound(ms: number): string {
  if (ms < 1_000) return `${ms} ms`;
  if (ms < 60_000) return `${ms / 1_000} s`;
  return `${ms / 60_000} min`;
}

/** Labels for each band, e.g. "≤100 ms", "≤300 ms", "≤1 s", ">1 s". */
export function bandLabels(group: FrequencyGroup): string[] {
  return [...group.bounds.map((b) => `≤${formatBound(b)}`), `>${formatBound(group.bounds[2])}`];
}

export function formatRatio(ratio: number): string {
  return `${ratio.toFixed(2)}×`;
}

export function operationLabel(operation: Pick<Operation, 'op' | 'variant'>): string {
  return operation.variant === 'default' ? `jj ${operation.op}` : `jj ${operation.op} (${operation.variant})`;
}

export function sizeLabel(size: Size): string {
  const fixture = provenance.fixtures[size];
  return `${size[0].toUpperCase()}${size.slice(1)}: ${fixture.name}`;
}

export const ISSUES_URL = 'https://github.com/joshka/girt/issues/';
export const COMMIT_URL = {
  girt: 'https://github.com/joshka/girt/commit/',
  jj: 'https://github.com/joshka/jj/commit/',
};
