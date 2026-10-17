// What each jj benchmark row runs, transcribed from `matrix_ops` and its setup helpers in
// scripts/jj-e2e/bench-ops.py. Keep this in step with the harness: a row without an entry fails the
// build, so a new benchmark cannot appear on the page without saying what it measures.

import type { Size } from './jj-performance';

export interface Command {
  /** Short display label replacing the harness variant name. Empty for the operation's only row. */
  label: string;
  /** The exact jj arguments, with `<file>` and `<commit>` standing for per-repository inputs. */
  jj: string;
  /** The repository state the command runs against, in plain words. */
  state: string;
  /** The git command timed alongside it, or null when Git has no equivalent. */
  git: string | null;
  /** Caveats about how jj and Git differ for this row. */
  note?: string;
}

// Every row starts from the same base unless its state says otherwise: a local clone of the
// repository, colocated with `jj git init --colocate`, so the working copy is an empty change on
// top of the repository's HEAD.
export const BASE_STATE =
  'Unless a row says otherwise, each command runs in a fresh copy of a local clone colocated with ' +
  '`jj git init --colocate`: the working copy is an empty change on top of the main branch.';

const DIRTY = '100 tracked files each have a line appended.';
const STACK10 = 'A stack of 10 changes, each adding one file, on top of the main branch.';
const EVOLUTION = 'The working-copy change has been described 30 times, giving 30 operations.';
const MOST_EDITED =
  'The tracked file changed in the most of the last 1,000 non-merge commits on the main branch.';
const LARGEST_COMMIT = 'The non-merge commit touching the most files in the last 3,000 on the main branch.';

const COMMANDS: Record<string, Command> = {
  'git init/colocate': {
    label: 'existing clone',
    jj: 'jj git init --colocate',
    state: 'A plain Git clone with no jj data yet; jj imports every ref and the history behind it.',
    git: null,
  },
  'git clone/local-full-checkout': {
    label: 'local path',
    jj: 'jj git clone origin.git repo',
    state: 'Clones a local bare copy of the repository over the local path, then checks out every file.',
    git: 'git clone -q --no-local origin.git repo',
  },
  'git import/100-external-refs': {
    label: '100 new Git branches',
    jj: 'jj --ignore-working-copy git import',
    state: '100 new branches were created with git update-ref, all at the main branch.',
    git: null,
  },
  'git export/100-pending-bookmarks': {
    label: '100 new bookmarks',
    jj: 'jj --ignore-working-copy git export',
    state:
      'A non-colocated repository with 100 bookmarks created by jj bookmark create and not yet written to Git.',
    git: 'git update-ref --stdin (100 branch creations in one transaction)',
  },
  'git fetch/noop': {
    label: 'nothing new',
    jj: 'jj git fetch',
    state: 'An untimed fetch has already imported the remote; the remote has not changed since.',
    git: 'git fetch -q',
  },
  'git fetch/1': {
    label: '1 new commit',
    jj: 'jj git fetch',
    state: 'After an untimed fetch, the remote main branch advanced by 1 commit adding one small file.',
    git: 'git fetch -q',
  },
  'git fetch/100': {
    label: '100 new commits',
    jj: 'jj git fetch',
    state: 'After an untimed fetch, the remote main branch advanced by 100 commits, each adding one small file.',
    git: 'git fetch -q',
  },
  'git fetch/tags': {
    label: '120 new tags',
    jj: 'jj git fetch',
    state: 'After an untimed fetch, the remote gained 100 lightweight and 20 annotated tags at its HEAD.',
    git: 'git fetch -q',
  },
  'git push/new': {
    label: 'new bookmark',
    jj: 'jj git push -b matrix-push',
    state: `${STACK10} Bookmark matrix-push is on the top change and does not exist on the remote.`,
    git: 'git push -q origin HEAD:refs/heads/matrix-push',
  },
  'git push/update': {
    label: 'move bookmark forward',
    jj: 'jj git push -b matrix-push',
    state: `${STACK10} The remote matrix-push is at the main branch, so the push moves it forward 10 commits.`,
    git: 'git push -q origin HEAD:refs/heads/matrix-push',
  },
  'git push/delete': {
    label: 'delete bookmark',
    jj: 'jj git push -b matrix-push',
    state: `${STACK10} matrix-push exists on the remote and was deleted locally, so the push deletes it.`,
    git: 'git push -q origin :refs/heads/matrix-push',
  },
  'git push/stale': {
    label: 'bookmark 1,000 commits behind',
    jj: 'jj git push -b matrix-push',
    state: `${STACK10} The remote matrix-push is 1,000 first-parent commits behind the main branch.`,
    git: 'git push -q origin HEAD:refs/heads/matrix-push',
  },
  'status/clean': {
    label: 'no changes',
    jj: 'jj status',
    state: 'Nothing changed in the working copy.',
    git: 'git status',
  },
  'status/small-edit': {
    label: '1 edited file',
    jj: 'jj status',
    state: 'One tracked file has a line appended.',
    git: 'git status',
  },
  'status/many-edits': {
    label: '1,000 edited files',
    jj: 'jj status',
    state: '1,000 tracked files each have a line appended (every file, 208, on the small repository).',
    git: 'git status',
  },
  'status/untracked': {
    label: '10,000 new identical files',
    jj: 'jj status',
    state: '10,000 new files in 100 directories, all with the same content, so jj stores one new blob.',
    git: 'git status',
  },
  'status/untracked-unique': {
    label: '10,000 new distinct files',
    jj: 'jj status',
    state: '10,000 new files in 100 directories, each with different content, so jj stores 10,000 new blobs.',
    git: 'git status',
  },
  'diff/wc': {
    label: '100 edited files',
    jj: 'jj diff',
    state: DIRTY,
    git: 'git diff',
  },
  'diff/stat': {
    label: '100 edited files, --stat',
    jj: 'jj diff --stat',
    state: DIRTY,
    git: 'git diff --stat',
  },
  'diff/git': {
    label: '100 edited files, --git',
    jj: 'jj diff --git',
    state: DIRTY,
    git: 'git diff',
  },
  'diff/large': {
    label: 'large historical commit',
    jj: 'jj diff --git -r <commit>',
    state: LARGEST_COMMIT,
    git: 'git diff <commit>^ <commit>',
  },
  'show/default': {
    label: 'large historical commit',
    jj: 'jj show <commit>',
    state: LARGEST_COMMIT,
    git: 'git show <commit>',
  },
  'file show/large-old-file': {
    label: 'most-edited file',
    jj: 'jj file show <file>',
    state: MOST_EDITED,
    git: 'git show HEAD:<file>',
  },
  'file annotate/large-old-file': {
    label: 'most-edited file',
    jj: 'jj file annotate <file>',
    state: MOST_EDITED,
    git: 'git blame -- <file>',
  },
  'restore/default': {
    label: '100 edited files',
    jj: 'jj restore',
    state: DIRTY,
    git: 'git restore .',
  },
  'log/default': {
    label: 'default view',
    jj: 'jj log',
    state: 'The default revset: the working copy, its unpushed ancestors and trunk.',
    git: 'git log --graph --oneline --decorate -n 20',
    note: 'jj and Git show different commits by default.',
  },
  'log/no-graph': {
    label: 'default view, no graph',
    jj: 'jj log --no-graph',
    state: 'The default revset, printed as a flat list.',
    git: 'git log --oneline -n 20',
    note: 'jj and Git show different commits by default.',
  },
  'log/limited': {
    label: 'latest 100 ancestors',
    jj: "jj log -r '::@' --limit 100",
    state: 'The graph of the 100 most recent ancestors of the working copy. Output goes to /dev/null.',
    git: 'git log --graph --oneline -n 100',
  },
  'log/ancestry': {
    label: 'whole history as a graph',
    jj: "jj log -r '::@'",
    state: 'The graph of every ancestor of the working copy, i.e. the full history. Output goes to /dev/null.',
    git: 'git log --graph --oneline',
  },
  'log/large': {
    label: 'every commit id',
    jj: `jj log -r 'all()' --no-graph -T 'commit_id ++ "\\n"'`,
    state: 'One line per commit reachable from any ref, no graph. Output goes to /dev/null.',
    git: 'git log --all --format=%H',
  },
  'evolog/default': {
    label: '',
    jj: 'jj evolog -r @ --limit 100',
    state: EVOLUTION,
    git: 'git reflog show -n 100 HEAD',
    note: 'The Git reflog only approximates jj’s change history.',
  },
  'op log/default': {
    label: '',
    jj: 'jj op log --limit 100',
    state: EVOLUTION,
    git: 'git reflog -n 100',
    note: 'jj operations and Git reflog entries are different records.',
  },
  'new/default': {
    label: '',
    jj: 'jj new',
    state: 'Creates a new empty change on top of the working copy.',
    git: 'git commit -q --allow-empty -m new',
  },
  'describe/default': {
    label: '',
    jj: 'jj describe -m matrix-described',
    state: `${STACK10} Describes the top change.`,
    git: 'git commit -q --amend --allow-empty -m matrix-described',
  },
  'commit/default': {
    label: '100 edited files',
    jj: 'jj commit -m matrix-commit',
    state: `${DIRTY} jj snapshots them and starts a new working-copy change.`,
    git: 'git commit -qam matrix-commit',
  },
  'squash/default': {
    label: '',
    jj: 'jj squash -m matrix-squash',
    state: `${STACK10} Squashes the top change into its parent.`,
    git: 'git reset --soft HEAD~2 && git commit -qm matrix-squash',
  },
  'split/default': {
    label: 'by file',
    jj: 'jj split -m matrix-split matrix-a.txt',
    state: 'One change adding three files; one of them is split into its own change.',
    git: 'git reset HEAD~1, then two git add and git commit',
  },
  'abandon/default': {
    label: '',
    jj: 'jj abandon @',
    state: `${STACK10} Abandons the top change.`,
    git: 'git reset --hard -q HEAD~1',
  },
  'edit/default': {
    label: '',
    jj: 'jj edit @-',
    state: `${STACK10} Moves the working copy to the change below the top.`,
    git: 'git checkout -q HEAD~1',
  },
  'duplicate/default': {
    label: '',
    jj: 'jj duplicate @-',
    state: `${STACK10} Copies the change below the top.`,
    git: 'git commit-tree plus git update-ref',
  },
  'rebase/1': {
    label: '1 change',
    jj: "jj rebase -s 'roots(<main>..@)' -d <main~20>",
    state: 'One change on top of the main branch, moved back onto the commit 20 first-parents earlier.',
    git: 'git rebase -q --onto <main~20> <main>',
  },
  'rebase/10': {
    label: '10 changes',
    jj: "jj rebase -s 'roots(<main>..@)' -d <main~20>",
    state: 'A stack of 10 changes on the main branch, moved back onto the commit 20 first-parents earlier.',
    git: 'git rebase -q --onto <main~20> <main>',
  },
  'rebase/100': {
    label: '100 changes',
    jj: "jj rebase -s 'roots(<main>..@)' -d <main~20>",
    state: 'A stack of 100 changes on the main branch, moved back onto the commit 20 first-parents earlier.',
    git: 'git rebase -q --onto <main~20> <main>',
  },
  'rebase/bookmark': {
    label: '10-change bookmark',
    jj: 'jj rebase -b matrix-topic -d matrix-main',
    state: `${STACK10} matrix-topic marks the top; matrix-main is the main branch plus one empty commit.`,
    git: 'git rebase -q matrix-main',
  },
  'undo/default': {
    label: '',
    jj: 'jj undo',
    state: `${EVOLUTION} Undoes the last description.`,
    git: null,
  },
  'op restore/default': {
    label: '',
    jj: 'jj op restore @-',
    state: `${EVOLUTION} Restores the repository to the operation before the last.`,
    git: null,
  },
  'bookmark/list': {
    label: 'list, all remotes',
    jj: 'jj bookmark list --all-remotes',
    state: 'Lists every local and remote bookmark the clone imported.',
    git: 'git branch -a',
  },
  'bookmark/create': {
    label: 'create',
    jj: 'jj bookmark create matrix-new',
    state: 'Creates a bookmark on the working copy.',
    git: 'git branch matrix-new',
  },
  'bookmark/move': {
    label: 'move back one',
    jj: 'jj bookmark set matrix-move -r @- --allow-backwards',
    state: 'Moves an existing bookmark from the working copy to its parent.',
    git: 'git branch -f matrix-move HEAD~1',
  },
  'bookmark/delete': {
    label: 'delete',
    jj: 'jj bookmark delete matrix-move',
    state: 'Deletes a bookmark created on the working copy during setup.',
    git: 'git branch -D matrix-move',
  },
  'util gc/clean': {
    label: 'nothing to collect',
    jj: 'jj util gc',
    state: 'No unreachable objects. Each backend repacks by its own policy, so the work differs.',
    git: 'git gc -q',
  },
  'util gc/garbage': {
    label: '2,000 unreachable blobs',
    jj: 'jj util gc --expire now',
    state: '2,000 unreachable loose blobs (8 MiB in total), dated 30 days old.',
    git: 'git gc -q --prune=now',
  },
  'workspace add/full-checkout': {
    label: 'full checkout',
    jj: 'jj workspace add ../added',
    state: 'Adds a second workspace and checks out every file.',
    git: 'git worktree add -q --detach ../added HEAD',
  },
};

/**
 * Per-repository inputs the harness picks from history (`facts` in bench-ops.py). They are fixed by
 * each fixture's HEAD, which provenance records.
 */
export const INPUTS: Record<Size, Record<string, string>> = {
  small: { file: 'src/cli_util.rs', commit: 'd8feed9be451053de8942c3cd6be21bbc8a1bd33' },
  medium: { file: 'object-file.c', commit: '47f79f619834acdd39d39bd1d3b33bf57f80d0a2' },
  large: { file: 'MAINTAINERS', commit: '81895a65ec63ee1daec3255dc1a06675d2fbe915' },
};

export function commandOf(op: string, variant: string): Command {
  const command = COMMANDS[`${op}/${variant}`];
  if (!command) {
    throw new Error(`jj benchmark row "${op} ${variant}" has no entry in jj-commands.ts`);
  }
  return command;
}

export interface Repository {
  /** Short name for column headers. */
  label: string;
  /** What the repository is and how the fixture differs from a plain clone. */
  description: string;
}

/**
 * The three benchmark repositories, described from the fixture builder's provenance records. Counts
 * of objects, refs and files come from docs/jj-performance-provenance.json; these entries only say
 * what each repository is.
 */
export const REPOSITORIES: Record<Size, Repository> = {
  small: {
    label: 'jj, Feb 2023',
    description:
      'jj’s own repository cut at commit 0ca4e2d (26 February 2023, shortly after jj 0.7.0): 2,847 commits, ' +
      'with main and the ten release tags v0.3.0 to v0.7.0 as its only refs. It is a small, old ' +
      'snapshot, not today’s jj.',
  },
  medium: {
    label: 'git.git + branches',
    description:
      'git.git at f57c9a7 (15 October 2026), 82,500 commits, plus 2,080 extra branches the fixture ' +
      'builder added, one at each of the 2,080 most recent first-parent commits. That gives it ' +
      '3,100 refs, far more than a plain clone of git.git.',
  },
  large: {
    label: 'Linux v6.1',
    description:
      'The Linux kernel at the v6.1 release (830b3c6, 11 December 2022), 1.14 million commits, with ' +
      'its own branches and tags and nothing added.',
  },
};
