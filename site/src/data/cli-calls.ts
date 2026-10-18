// The library items each CLI example exercises, shown in the rail beside the example. Paths are
// relative to the girt docs.rs root; check them against `cargo doc --all-features` when the CLI
// or the library's public paths change.

export interface CliCalls {
  /** The girt command or commands the example runs; defaults to `girt <key>`. */
  command?: string;
  /** The nearest Git command, when there is one. */
  git?: string;
  /** Library items as `[label, path]`; a full `https://` URL is used as is, as for an
   * unreleased item linked to its issue. */
  api: [string, string][];
}

const repository = (method: string): [string, string] => [
  `Repository::${method}`,
  `repository/struct.Repository.html#method.${method}`,
];
const objects = (method: string): [string, string] => [
  `Objects::${method}`,
  `objects/struct.Objects.html#method.${method}`,
];
const method = (type: string, path: string, name: string): [string, string] => [
  `${type}::${name}`,
  `${path}#method.${name}`,
];

const resolver = (name: string) => method('Resolver', 'revision/struct.Resolver.html', name);
const references = (name: string) => method('References', 'refs/struct.References.html', name);
const remote = (name: string) => method('Remote', 'remote/struct.Remote.html', name);
// Items not yet in a release link the issue that tracks their area until docs.rs has them.
const unreleased = (label: string, issue: number): [string, string] => [
  label,
  `https://github.com/joshka/girt/issues/${issue}`,
];
const configEdit = (name: string) => method('Document', 'config/struct.Document.html', name);

export const calls: Record<string, CliCalls> = {
  // Repositories and objects.
  init: {
    git: 'git init',
    api: [
      repository('init_with_options'),
      method('InitOptions', 'repository/struct.InitOptions.html', 'defaults_from'),
      method('InitOptions', 'repository/struct.InitOptions.html', 'with_initial_branch'),
    ],
  },
  'repo info': {
    git: 'git rev-parse --git-dir …',
    api: [
      repository('discover_with_config'),
      repository('object_format'),
      repository('reference_backend'),
      repository('common_dir'),
      repository('shallow_roots'),
      repository('operation_state'),
    ],
  },
  'object read': {
    command: 'girt object id, info, show, cat',
    git: 'git rev-parse --verify, git cat-file',
    api: [
      method('Repo', 'repo/struct.Repo.html', 'rev_parse'),
      objects('read_with'),
      method('Object', 'objects/struct.Object.html', 'kind'),
      method('Object', 'objects/struct.Object.html', 'data'),
    ],
  },
  'object batch': {
    git: 'git cat-file --batch',
    api: [resolver('resolve'), objects('read_with')],
  },
  'object hash': {
    git: 'git hash-object',
    api: [
      method('ObjectId', 'object/enum.ObjectId.html', 'for_blob'),
      method('LooseObjects', 'loose/struct.LooseObjects.html', 'write_blob'),
      method('Tree', 'tree/struct.Tree.html', 'parse'),
      method('Commit', 'commit/struct.Commit.html', 'parse'),
    ],
  },
  'object list': {
    command: 'girt object list, count',
    git: 'git cat-file --batch-all-objects, git count-objects',
    api: [
      objects('object_ids'),
      objects('headers'),
      objects('stores'),
      method('ObjectStore', 'objects/struct.ObjectStore.html', 'packs'),
    ],
  },
  'tree list': {
    git: 'git ls-tree',
    api: [resolver('resolve_to'), objects('read_tree_with'), method('Tree', 'tree/struct.Tree.html', 'entries')],
  },

  // References.
  'ref list': {
    git: 'git for-each-ref',
    api: [repository('references'), references('list'), references('resolve')],
  },
  'ref show': {
    git: 'git rev-parse <name>',
    api: [['revision::ref_candidates', 'revision/fn.ref_candidates.html'], references('read')],
  },
  'ref set': {
    git: 'git update-ref',
    api: [
      references('transaction'),
      method('RefEdit', 'refs/struct.RefEdit.html', 'new'),
      ['Expected', 'refs/enum.Expected.html'],
      repository('reflog_for'),
    ],
  },
  'ref delete': {
    git: 'git update-ref -d',
    api: [references('transaction'), method('RefEdit', 'refs/struct.RefEdit.html', 'new'), ['Reflog', 'refs/enum.Reflog.html']],
  },
  'replace graft': {
    git: 'git replace --graft',
    api: [
      unreleased('Repository::replace_graft', 529),
      unreleased('Repository::replacements', 529),
      unreleased('ReplacedObjects::walk', 529),
    ],
  },
  'ref log': {
    git: 'git reflog show',
    api: [references('reflog'), objects('abbreviate')],
  },

  // History, diffs and blame.
  log: {
    git: 'git log',
    api: [resolver('select'), objects('walk_with'), objects('read_commit_with')],
  },
  'history count': {
    git: 'git rev-list --count',
    api: [resolver('select'), objects('walk_with')],
  },
  'history merge-base': {
    git: 'git merge-base',
    api: [objects('merge_bases_with')],
  },
  'history describe': {
    git: 'git describe',
    api: [
      unreleased('Repository::describe', 511),
      unreleased('DescribeOptions::with_lightweight_tags', 511),
      unreleased('Description::format', 511),
    ],
  },
  'history contains': {
    git: 'git merge-base --is-ancestor',
    api: [objects('is_ancestor_with')],
  },
  diff: {
    git: 'git diff-tree -r --name-status',
    api: [
      objects('compare_trees'),
      objects('diff_trees_with'),
      method('rewrites::Options', 'rewrites/struct.Options.html', 'with_copies'),
    ],
  },
  'diff patch': {
    command: 'girt diff --patch',
    git: 'git diff',
    api: [objects('write_patch_with'), repository('diff_attributes')],
  },
  blame: {
    git: 'git blame',
    api: [method('Repo', 'repo/struct.Repo.html', 'blame')],
  },
  archive: {
    git: 'git archive',
    api: [
      unreleased('Repository::archive', 526),
      unreleased('archive::Options', 526),
    ],
  },
  grep: {
    git: 'git grep',
    api: [
      unreleased('Repository::grep', 525),
      unreleased('Searcher::search', 525),
      unreleased('GitFormat::write', 525),
    ],
  },

  // Index and working tree.
  'index list': {
    git: 'git ls-files',
    api: [repository('read_index'), method('Index', 'index/struct.Index.html', 'entries')],
  },
  'index refresh': {
    git: 'git update-index --refresh',
    api: [
      unreleased('Repository::refresh_index', 653),
      unreleased('Repository::refresh_index_with', 653),
    ],
  },
  status: {
    git: 'git status --porcelain',
    api: [
      repository('status'),
      ['status::Baseline', 'status/enum.Baseline.html'],
      ['status::UntrackedFiles', 'status/enum.UntrackedFiles.html'],
      unreleased('Objects::ahead_behind', 513),
      unreleased('Resolver::upstream_name', 513),
    ],
  },
  'ignore check': {
    git: 'git check-ignore -v',
    api: [repository('ignore_rules'), method('WorktreeRules', 'ignore/struct.WorktreeRules.html', 'check')],
  },
  checkout: {
    git: 'git read-tree -m -u',
    api: [repository('checkout_tree'), repository('checkout_tree_with_progress')],
  },
  reset: {
    git: 'git reset',
    api: [
      unreleased('Repository::reset', 514),
      unreleased('Repository::reset_paths', 514),
      unreleased('reset::ResetMode', 514),
    ],
  },
  worktree: {
    command: 'girt worktree list, lock, unlock, prune, repair',
    git: 'git worktree',
    api: [
      repository('worktrees'),
      repository('lock_worktree'),
      repository('unlock_worktree'),
      repository('prune_worktree'),
      repository('repair_worktree'),
    ],
  },

  // Configuration and remotes.
  'config read': {
    command: 'girt config get, list',
    git: 'git config --get, --list',
    api: [repository('config'), method('Config', 'config/struct.Config.html', 'value'), method('Config', 'config/struct.Config.html', 'entries')],
  },
  'config write': {
    command: 'girt config set, unset',
    git: 'git config --local, --local --unset-all',
    api: [
      repository('edit_config'),
      configEdit('set_value'),
      configEdit('remove'),
      method('ConfigEdit', 'config/struct.ConfigEdit.html', 'commit'),
    ],
  },
  remote: {
    command: 'girt remote list, add, show',
    git: 'git remote',
    api: [
      remote('names'),
      remote('find'),
      remote('fetch_refspecs'),
      method('RemoteConfig', 'remote/struct.RemoteConfig.html', 'add'),
    ],
  },
  'remote heads': {
    git: 'git ls-remote',
    api: [
      ['fetch::discover_local', 'fetch/fn.discover_local.html'],
      ['fetch::discover_http', 'fetch/fn.discover_http.html'],
      ['fetch::discover_ssh', 'fetch/fn.discover_ssh.html'],
    ],
  },
  clone: {
    git: 'git clone',
    api: [
      ['workflow::clone_with', 'workflow/fn.clone_with.html'],
      method('CloneOptions', 'workflow/struct.CloneOptions.html', 'with_checkout'),
      ['clone::CloneHead', 'clone/enum.CloneHead.html'],
    ],
  },
  fetch: {
    git: 'git fetch',
    api: [['workflow::fetch', 'workflow/fn.fetch.html'], method('FetchOptions', 'workflow/struct.FetchOptions.html', 'new'), remote('fetch_refspecs')],
  },
  push: {
    git: 'git push --force-with-lease',
    api: [
      ['workflow::push', 'workflow/fn.push.html'],
      method('PushCommand', 'push/struct.PushCommand.html', 'new'),
      method('PushCommand', 'push/struct.PushCommand.html', 'with_force'),
      method('PushReport', 'push/struct.PushReport.html', 'all_succeeded'),
    ],
  },

  // Packs and maintenance.
  'pack list': {
    api: [
      method('ObjectStore', 'objects/struct.ObjectStore.html', 'packs'),
      method('IndexedPack', 'objects/struct.IndexedPack.html', 'object_count'),
      method('IndexedPack', 'objects/struct.IndexedPack.html', 'is_kept'),
    ],
  },
  'pack verify': {
    git: 'git verify-pack',
    api: [objects('verify_packs')],
  },
  'pack reindex': {
    git: 'git index-pack',
    api: [['pack::rebuild_index', 'pack/fn.rebuild_index.html']],
  },
  'maintenance plan': {
    api: [repository('plan_retention'), method('RetentionPolicy', 'retention/struct.RetentionPolicy.html', 'trusted')],
  },
  'maintenance repack': {
    api: [repository('repack_retained')],
  },
  'maintenance run': {
    api: [repository('expire_reflogs'), repository('retire_old_packs'), repository('prune_unreachable_loose')],
  },
};
