// Build-time view of the committed jj benchmark matrix. The numbers come only from the JSON files in
// the repository's docs/ directory; this module classifies and formats them for perf-render.ts.
import rows from '../../../docs/jj-performance.json';
import provenanceJson from '../../../docs/jj-performance-provenance.json';
import { commandOf, INPUTS, REPOSITORIES } from './jj-commands';
import type { CellData, ColumnData, Frequency, Payload, RowData, Size, Status } from './perf-render';

export type { Size } from './perf-render';
export { formatMs } from './perf-render';
export const SIZES: Size[] = ['small', 'medium', 'large'];

export interface Row {
  repo: string;
  size: Size;
  op: string;
  variant: string;
  status?: string;
  tool_status?: Record<string, string>;
  girt_ms: number | null;
  stock_ms: number | null;
  git_ms: number | null;
  girt_samples: number;
  stock_samples?: number;
  git_samples?: number;
  ratio_vs_stock: number | null;
  jj_level_gap: boolean;
  issues?: number[];
  notes: string;
  sample_ranges_ms?: Partial<Record<'girt' | 'stock' | 'git', [number, number]>>;
  timeout_limits_s?: Record<string, number>;
  run_epochs?: string[];
  girt_version?: string;
  girt_commit?: string;
  measured_on?: string;
  /** False when girt and stock jj do different work, so the times are not a speed comparison. */
  comparable?: boolean;
  comparability_note?: string;
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
  girt_release?: string;
  notes?: string[];
  load_1m?: { min: number; median: number; max: number; cores: number };
}

export const provenance = provenanceJson as unknown as Provenance;

const versionKey = (version: string) => version.split(/[.+]/).map((part) => Number.parseInt(part, 36));
const compareVersions = (a: string, b: string) => {
  const [x, y] = [versionKey(a), versionKey(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const diff = (x[i] ?? -1) - (y[i] ?? -1);
    if (diff) return diff;
  }
  return 0;
};

/** The girt releases and measurement dates the rows span, oldest first. */
export function spanOf(rows: Pick<Row, 'girt_version' | 'measured_on'>[]) {
  const versions = [...new Set(rows.flatMap((row) => (row.girt_version ? [row.girt_version] : [])))].sort(compareVersions);
  const dates = [...new Set(rows.flatMap((row) => (row.measured_on ? [row.measured_on] : [])))].sort();
  const range = (values: string[]) =>
    values.length === 0 ? null : values.length === 1 ? values[0] : `${values[0]}–${values[values.length - 1]}`;
  return { versions: range(versions), dates: range(dates) };
}
const committedRows = rows as unknown as Row[];

/** The girt releases and dates the committed matrix spans. */
export const matrixSpan = spanOf(committedRows);

/** girt is responsible for a slow cell when it is at least this much slower than stock jj. */
export const GIRT_RESPONSIBLE_RATIO = 1.2;

export interface FrequencyGroup {
  id: Frequency;
  title: string;
  description: string;
}

export const GROUPS: FrequencyGroup[] = [
  {
    id: 'constant',
    title: 'Constantly',
    description: 'run many times an hour while working: inspecting and editing changes',
  },
  {
    id: 'hourly',
    title: 'Hourly',
    description: 'run several times a day: syncing with remotes and reshaping history',
  },
  {
    id: 'occasional',
    title: 'Occasionally',
    description: 'setup, maintenance and full-history dumps',
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

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** "14 Oct 2026", or "14–16 Oct 2026" for a span within a month. */
function datesOf(epochs: string[]): string {
  const days = [...new Set(epochs.map((epoch) => epoch.slice(0, 10)))].sort();
  if (!days.length) return 'unknown date';
  const parse = (day: string) => day.split('-').map(Number);
  const [y1, m1, d1] = parse(days[0]);
  const [y2, m2, d2] = parse(days.at(-1)!);
  const end = `${d2} ${MONTHS[m2 - 1]} ${y2}`;
  if (days.length === 1) return end;
  return y1 === y2 && m1 === m2 ? `${d1}–${end}` : `${d1} ${MONTHS[m1 - 1]} ${y1} to ${end}`;
}

function statusOf(row: Row): Status {
  const status = row.status ?? 'ok';
  if (status === 'ok' && row.girt_ms != null) return 'ok';
  if (status === 'timeout') return 'timeout';
  if (status === 'unsupported' || row.tool_status?.girt === 'refusal') return 'refused';
  return 'missing';
}

/** Notes that describe the workload, without per-tool sample ranges or failure details. */
function workloadNotes(notes: string): string[] {
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

function cellOf(row: Row): CellData {
  return {
    g: row.girt_ms,
    s: row.stock_ms,
    x: row.git_ms,
    status: statusOf(row),
    ranges: row.sample_ranges_ms ?? {},
    samples: { girt: row.girt_samples, stock: row.stock_samples ?? null, git: row.git_samples ?? null },
    issues: row.issues ?? [],
    timeoutS: row.timeout_limits_s?.girt ?? null,
    when: datesOf(row.run_epochs ?? []),
    version: row.girt_version ?? null,
    comparable: row.comparable !== false,
    comparabilityNote: row.comparability_note ?? null,
    stale: Boolean(row.girt_version && provenance.girt_release && row.girt_version !== provenance.girt_release),
    notes: workloadNotes(row.notes),
  };
}

function buildRows(): RowData[] {
  const byKey = new Map<string, RowData>();
  for (const row of committedRows) {
    const key = `${row.op}/${row.variant}`;
    let data = byKey.get(key);
    if (!data) {
      data = {
        key,
        op: row.op,
        variant: row.variant,
        freq: frequencyOf(row.op, row.variant),
        ...commandOf(row.op, row.variant),
        inputs: {},
        cells: {},
      };
      byKey.set(key, data);
    }
    data.cells[row.size] = cellOf(row);
  }
  for (const data of byKey.values()) {
    // Notes every size shares describe the workload, which the row's state already says; keep
    // only the details specific to one repository.
    const cells = Object.values(data.cells);
    const shared = cells[0].notes.filter((note) => cells.every((cell) => cell.notes.includes(note)));
    for (const cell of cells) cell.notes = cell.notes.filter((note) => !shared.includes(note));
    for (const size of SIZES) {
      const inputs = Object.entries(INPUTS[size]).filter(([name]) =>
        `${data.jj} ${data.git ?? ''}`.includes(`<${name}>`),
      );
      if (inputs.length) data.inputs[size] = Object.fromEntries(inputs);
    }
  }
  return [...byKey.values()];
}

export const perfRows = buildRows();

function columnOf(size: Size): ColumnData {
  const fixture = provenance.fixtures[size];
  const epochs = committedRows.filter((row) => row.size === size).flatMap((row) => row.run_epochs ?? []);
  return {
    size,
    name: REPOSITORIES[size].label,
    objects: Number(fixture.objects),
    files: fixture.worktree_files,
    source: `measured ${datesOf(epochs)}`,
  };
}

export const ISSUES_URL = 'https://github.com/joshka/girt/issues/';

export const payload: Payload = {
  rows: perfRows,
  columns: SIZES.map(columnOf),
  groups: GROUPS,
  issuesUrl: ISSUES_URL,
};

export const COMMIT_URL = {
  girt: 'https://github.com/joshka/girt/commit/',
  jj: 'https://github.com/joshka/jj/commit/',
};
