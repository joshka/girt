// Renders the jj benchmark matrix to HTML. The page renders the default view at build time and the
// browser re-renders with the same code when a reader changes a control, so this module must stay
// free of JSON imports and Node APIs.

export type Size = 'small' | 'medium' | 'large';
export type Frequency = 'constant' | 'hourly' | 'occasional';
export type Status = 'ok' | 'refused' | 'timeout' | 'missing';
type Tool = 'girt' | 'stock' | 'git';

export interface CellData {
  g: number | null;
  s: number | null;
  x: number | null;
  status: Status;
  ranges: Partial<Record<Tool, [number, number]>>;
  samples: Record<Tool, number | null>;
  issues: number[];
  timeoutS: number | null;
  /** When this cell was measured. */
  when: string;
  /** girt release the cell was measured on, if recorded. */
  version: string | null;
  /** Whether that release differs from the release most cells were measured on. */
  stale: boolean;
  /** False when girt and the comparison do different work; such cells have no extra time. */
  comparable: boolean;
  comparabilityNote: string | null;
  notes: string[];
}

export interface RowData {
  key: string;
  op: string;
  variant: string;
  freq: Frequency;
  label: string;
  jj: string;
  state: string;
  git: string | null;
  note?: string;
  inputs: Partial<Record<Size, Record<string, string>>>;
  cells: Partial<Record<Size, CellData>>;
}

export interface ColumnData {
  size: Size;
  name: string;
  objects: number;
  files: number;
  /** When this column was measured. */
  source: string;
}

export interface GroupData {
  id: Frequency;
  title: string;
  description: string;
}

export interface Payload {
  rows: RowData[];
  columns: ColumnData[];
  groups: GroupData[];
  issuesUrl: string;
}

export interface ViewState {
  /** Hide rows whose time differs by less than this everywhere, in milliseconds; 0 shows all. */
  threshold: number;
  arrange: 'workflow' | 'cost';
  colour: 'extra' | 'ratio';
  base: 'stock' | 'git';
  open: string[];
}

export const DEFAULT_STATE: ViewState = {
  threshold: 100,
  arrange: 'workflow',
  colour: 'extra',
  base: 'stock',
  open: [],
};

export const THRESHOLDS = [0, 50, 100, 500, 2_000, 10_000];

export function formatMs(ms: number): string {
  if (ms < 1_000) return `${Math.round(ms)} ms`;
  if (ms < 10_000) return `${(ms / 1_000).toFixed(2)} s`;
  if (ms < 60_000) return `${(ms / 1_000).toFixed(1)} s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.round((ms % 60_000) / 1_000);
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`;
}

export function formatThreshold(ms: number): string {
  if (ms === 0) return 'show all';
  return ms < 1_000 ? `${ms} ms` : `${ms / 1_000} s`;
}

const formatCount = (n: number) =>
  n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n);

export const formatExtra = (ms: number) => `${ms >= 0 ? '+' : '−'}${formatMs(Math.abs(ms))}`;

const escape = (text: string) =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

export const baseOf = (cell: CellData, base: ViewState['base']) => (base === 'git' ? cell.x : cell.s);

/** Extra wall-clock time jj on girt takes over the comparison, or null when either is missing. */
export function extraOf(cell: CellData | undefined, base: ViewState['base']): number | null {
  if (!cell || cell.status !== 'ok' || cell.g == null || !cell.comparable) return null;
  const other = baseOf(cell, base);
  return other == null ? null : cell.g - other;
}

/** The largest extra time across sizes; a failed cell counts as worse than any time. */
export function worstOf(row: RowData, base: ViewState['base']): number {
  let worst = -Infinity;
  for (const cell of Object.values(row.cells)) {
    if (cell.status !== 'ok') return Infinity;
    worst = Math.max(worst, extraOf(cell, base) ?? -Infinity);
  }
  return worst;
}

/** The largest difference in either direction; a failure or a missing comparison always shows. */
function largestChange(row: RowData, base: ViewState['base']): number {
  let largest = 0;
  for (const cell of Object.values(row.cells)) {
    const extra = extraOf(cell, base);
    if (extra == null) return Infinity;
    largest = Math.max(largest, Math.abs(extra));
  }
  return largest;
}

export const isVisible = (row: RowData, state: ViewState) =>
  state.threshold === 0 || largestChange(row, state.base) >= state.threshold;

// Human-scale shades: what an extra wait feels like, whatever the ratio.
const EXTRA_SHADES: [number, string][] = [
  [100, 'under 0.1 s: not noticeable'],
  [500, '0.1 to 0.5 s: noticeable'],
  [2_000, '0.5 to 2 s: a pause'],
  [10_000, '2 to 10 s: slow'],
  [Infinity, 'over 10 s: painful'],
];
const RATIO_SHADES: [number, string][] = [
  [1.2, 'within 1.2×'],
  [1.5, '1.2 to 1.5×'],
  [2, '1.5 to 2×'],
  [3, '2 to 3×'],
  [Infinity, 'over 3×'],
];

/** Shade 0 is plain and 4 the darkest; 'win' marks commands girt makes faster. */
export function shadeOf(cell: CellData | undefined, state: ViewState): string {
  if (!cell) return '0';
  if (cell.status !== 'ok') return '4';
  if (!cell.comparable) return '0';
  const other = baseOf(cell, state.base);
  if (cell.g == null || other == null) return '0';
  if (state.colour === 'extra') {
    const extra = cell.g - other;
    if (extra <= -100) return 'win';
    return String(EXTRA_SHADES.findIndex(([bound]) => extra < bound));
  }
  const ratio = cell.g / other;
  if (ratio <= 1 / 1.2) return 'win';
  return String(RATIO_SHADES.findIndex(([bound]) => ratio < bound));
}

export function renderLegend(state: ViewState): string {
  const shades = state.colour === 'extra' ? EXTRA_SHADES : RATIO_SHADES;
  const win = state.colour === 'extra' ? 'faster by 0.1 s or more' : '1.2× faster or better';
  return [
    `<span class="jjp-swatch" data-shade="win">${win}</span>`,
    ...shades.map(([, label], index) => `<span class="jjp-swatch" data-shade="${index}">${label}</span>`),
  ].join('');
}

const baseName = (state: ViewState) => (state.base === 'git' ? 'git' : 'stock');

export function rowLabel(row: Pick<RowData, 'op' | 'label'>): string {
  return `<span class="jjp-op">jj ${escape(row.op)}</span>${row.label ? ` <span class="jjp-variant">${escape(row.label)}</span>` : ''}`;
}

function cellHtml(row: RowData, column: ColumnData, state: ViewState): string {
  const cell = row.cells[column.size];
  if (!cell) return '<td><div class="jjp-cell jjp-na">not measured</div></td>';
  const attrs = `class="jjp-cell" tabindex="0" data-key="${escape(row.key)}" data-size="${column.size}" data-shade="${shadeOf(cell, state)}"`;
  const other = baseOf(cell, state.base);
  const otherText = other == null ? `no ${baseName(state)} time` : `${baseName(state)} ${formatMs(other)}`;
  if (cell.status !== 'ok') {
    const what =
      cell.status === 'timeout'
        ? `timed out${cell.timeoutS ? ` at ${formatMs(cell.timeoutS * 1_000)}` : ''}`
        : cell.status === 'refused'
          ? 'refused'
          : 'no result';
    const staleFailure = cell.stale ? `<span class="jjp-sub">measured on girt ${escape(cell.version!)}</span>` : '';
    return `<td><div ${attrs}><span class="jjp-time">${what}</span><span class="jjp-sub">${otherText}</span>${staleFailure}</div></td>`;
  }
  const ratio = other ? ` · ${(cell.g! / other).toFixed(2)}×` : '';
  const stale = cell.stale ? `<span class="jjp-sub">measured on girt ${escape(cell.version!)}</span>` : '';
  const unlike = cell.comparable ? '' : '<span class="jjp-sub">not like-for-like</span>';
  return `<td><div ${attrs}><span class="jjp-time">${formatMs(cell.g!)}</span><span class="jjp-sub">${otherText}${ratio}</span>${unlike}${stale}</div></td>`;
}

/** "64–66 ms", or "2 min 7 s – 2 min 13 s" when the units differ. */
function formatRange([low, high]: [number, number]): string {
  const [a, b] = [formatMs(low), formatMs(high)];
  const unit = (text: string) => text.replace(/^[\d.]+/, '');
  return /^[\d.]+ (ms|s)$/.test(a) && unit(a) === unit(b) ? `${a.replace(/ \S+$/, '')}–${b}` : `${a} – ${b}`;
}

function range(cell: CellData, tool: Tool): string {
  const r = cell.ranges[tool];
  const n = cell.samples[tool];
  const parts = [r && r[0] !== r[1] ? formatRange(r) : null, n ? `${n} run${n === 1 ? '' : 's'}` : null];
  const text = parts.filter(Boolean).join(', ');
  return text ? ` <span class="jjp-muted">${text}</span>` : '';
}

function timeOf(cell: CellData, tool: Tool): string {
  const ms = tool === 'girt' ? cell.g : tool === 'stock' ? cell.s : cell.x;
  if (tool === 'girt' && cell.status !== 'ok') {
    return cell.status === 'timeout' && cell.timeoutS
      ? `timed out at ${formatMs(cell.timeoutS * 1_000)}`
      : cell.status;
  }
  return ms == null ? '–' : `${formatMs(ms)}${range(cell, tool)}`;
}

function issueLinks(payload: Payload, issues: number[]): string {
  return issues.map((n) => `<a href="${payload.issuesUrl}${n}">#${n}</a>`).join(' ');
}

function detailHtml(payload: Payload, row: RowData): string {
  const git = row.git
    ? `<p><span class="jjp-muted">Timed against</span> <code>${escape(row.git)}</code></p>`
    : '<p class="jjp-muted">Git has no equivalent command, so there is no git time.</p>';
  const sizes = payload.columns
    .map((column) => {
      const cell = row.cells[column.size];
      if (!cell) return '';
      const inputs = Object.entries(row.inputs[column.size] ?? {})
        .map(([name, value]) => `<code>&lt;${name}&gt;</code> = <code>${escape(name === 'commit' ? value.slice(0, 12) : value)}</code>`)
        .join(', ');
      const extra = extraOf(cell, 'stock');
      const notes = [inputs, cell.comparabilityNote && escape(cell.comparabilityNote), ...cell.notes.map(escape)]
        .filter(Boolean)
        .join('; ');
      return `<tr>
        <th scope="row">${column.size}<span class="jjp-muted">${escape(column.name)}</span><span class="jjp-muted">${escape(cell.when)}${cell.version ? `, girt ${escape(cell.version)}` : ''}</span></th>
        <td>${timeOf(cell, 'girt')}</td>
        <td>${timeOf(cell, 'stock')}</td>
        <td>${timeOf(cell, 'git')}</td>
        <td>${extra == null ? '–' : formatExtra(extra)}</td>
        <td>${issueLinks(payload, cell.issues)}</td>
        <td class="jjp-notes">${notes}</td>
      </tr>`;
    })
    .join('');
  return `<div class="jjp-detail-body">
    <p><span class="jjp-muted">Runs</span> <code>${escape(row.jj)}</code></p>
    <p>${escape(row.state)}${row.note ? ` ${escape(row.note)}` : ''}</p>
    ${git}
    <div class="jjp-scroll"><table class="jjp-runs">
      <thead><tr><th>repository, measured</th><th>jj on girt</th><th>stock jj</th><th>git</th><th>extra vs stock</th><th>tracking</th><th>inputs and notes</th></tr></thead>
      <tbody>${sizes}</tbody>
    </table></div>
  </div>`;
}

function rowHtml(payload: Payload, row: RowData, state: ViewState): string {
  const open = state.open.includes(row.key);
  const head = `<tr class="jjp-row${open ? ' jjp-open' : ''}">
    <th scope="row"><button type="button" class="jjp-toggle" data-key="${escape(row.key)}" aria-expanded="${open}">
      <span class="jjp-chevron" aria-hidden="true"></span>${rowLabel(row)}<code class="jjp-cmd">${escape(row.jj)}</code>
    </button></th>
    ${payload.columns.map((column) => cellHtml(row, column, state)).join('')}
  </tr>`;
  return open
    ? `${head}<tr class="jjp-detail"><td colspan="${payload.columns.length + 1}">${detailHtml(payload, row)}</td></tr>`
    : head;
}

export function renderMatrix(payload: Payload, state: ViewState): string {
  const byWorst = (a: RowData, b: RowData) =>
    worstOf(b, state.base) - worstOf(a, state.base) || a.key.localeCompare(b.key);
  const visible = payload.rows.filter((row) => isVisible(row, state)).sort(byWorst);
  const head = `<thead><tr><th scope="col" class="jjp-cmdhead">command</th>${payload.columns
    .map(
      (column) => `<th scope="col" class="jjp-size">${column.size}
        <span class="jjp-colsub">${escape(column.name)}</span>
        <span class="jjp-colsub">${formatCount(column.objects)} objects</span>
        <span class="jjp-colsub">${escape(column.source)}</span></th>`,
    )
    .join('')}</tr></thead>`;
  let body = '';
  if (state.arrange === 'workflow') {
    for (const group of payload.groups) {
      const rows = visible.filter((row) => row.freq === group.id);
      if (!rows.length) continue;
      body += `<tr class="jjp-group"><th colspan="${payload.columns.length + 1}" scope="rowgroup">${escape(group.title)} <span class="jjp-muted">${escape(group.description)}</span></th></tr>`;
      body += rows.map((row) => rowHtml(payload, row, state)).join('');
    }
  } else {
    body = visible.map((row) => rowHtml(payload, row, state)).join('');
  }
  return head + `<tbody>${body}</tbody>`;
}

export function renderHiddenNote(payload: Payload, state: ViewState): string {
  const hidden = payload.rows.filter((row) => !isVisible(row, state)).length;
  if (!hidden) return '';
  const base = state.base === 'git' ? 'the git CLI' : 'stock jj';
  return `${hidden} more commands are within ${formatThreshold(state.threshold)} of ${base} on every repository. <button type="button" data-threshold="0">Show them</button>`;
}

export function tooltipHtml(row: RowData, size: Size, state: ViewState): string {
  const cell = row.cells[size];
  if (!cell) return '';
  const extra = extraOf(cell, state.base);
  return `<b>${rowLabel(row)}</b> <span class="jjp-muted">on ${size}</span><br>
    jj on girt ${timeOf(cell, 'girt')}<br>
    stock jj&nbsp;&nbsp; ${timeOf(cell, 'stock')}<br>
    git&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp; ${timeOf(cell, 'git')}<br>
    ${extra == null ? '' : `extra wait <b>${formatExtra(extra)}</b> vs ${baseName(state)}<br>`}
    ${cell.comparabilityNote ? `${escape(cell.comparabilityNote)}<br>` : ''}
    ${cell.issues.length ? `tracking ${cell.issues.map((n) => `#${n}`).join(', ')}<br>` : ''}
    <span class="jjp-muted">measured ${escape(cell.when)}${cell.version ? ` on girt ${escape(cell.version)}` : ''}; click the command for details</span>`;
}
