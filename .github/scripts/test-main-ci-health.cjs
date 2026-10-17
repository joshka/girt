// Exercise the actual inline reporter with GitHub API fakes, without network or credentials.
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const {join} = require('node:path');
const source = readFileSync(join(__dirname, '../workflows/main-ci-health.yml'), 'utf8')
  .split('          script: |\n')[1].split('\n').map(line => line.slice(12)).join('\n');
const report = new (Object.getPrototypeOf(async function() {}).constructor)('github', 'context', 'core', source);

async function run({conclusion = 'failure', latestId = 10, latestAttempt = 1,
                    latestStatus = 'completed', latestEvent = 'push', path = '.github/workflows/validation.yml',
                    issue, jobs = [], apiError = false} = {}) {
  const writes = [];
  const current = {path, id: 10, run_attempt: 1, head_sha: 'checked-sha', conclusion,
    html_url: 'https://example.test/run/10'};
  const actions = {
    listWorkflowRuns: async args => {
      assert.equal(args.workflow_id, 'validation.yml');
      assert.equal(args.branch, 'main');
      assert.equal(args.per_page, 1);
      if (apiError) throw new Error('API unavailable');
      return {data: {workflow_runs: args.event === latestEvent ?
        [{id: latestId, run_attempt: latestAttempt, status: latestStatus}] : []}};
    },
    listJobsForWorkflowRun: 'jobs',
  };
  const issues = {listForRepo: 'issues',
    update: async args => writes.push(['update', args]),
    create: async args => writes.push(['create', args]),
    createComment: async args => writes.push(['comment', args]),
  };
  const github = {rest: {actions, issues}, paginate: async (method, args) => {
    if (method === 'jobs') {
      assert.equal(args.run_id, 10);
      assert.equal(args.filter, 'latest');
      return jobs;
    }
    assert.equal(args.state, 'all');
    assert.equal(args.creator, 'github-actions[bot]');
    return issue ? [issue] : [];
  }};
  await report(github, {repo: {owner: 'test', repo: 'girt'},
    payload: {workflow_run: current}}, {info() {}});
  return writes;
}

(async () => {
  const issue = {number: 7, title: 'CI regression on main', state: 'closed',
    user: {login: 'github-actions[bot]'}};
  const jobs = [{name: 'Test (macos-14)', conclusion: 'failure',
    html_url: 'https://example.test/job/1'}];
  const created = await run({jobs});
  assert.equal(created[0][0], 'create');
  assert.match(created[0][1].body, /\[Test \(macos-14\)\]\(https:\/\/example.test\/job\/1\): failure/);
  const reopened = await run({issue, jobs});
  assert.equal(reopened[0][0], 'update');
  assert.equal(reopened[0][1].state, 'open');
  assert.equal(reopened[0][1].issue_number, 7);
  const recovered = await run({issue: {...issue, state: 'open'}, conclusion: 'success', jobs: [{conclusion: 'skipped'}]});
  assert.equal(recovered[0][1].state, 'closed');
  assert.match(recovered[0][1].body, /Recovered at checked-sha/);
  assert.deepEqual(await run({conclusion: 'success'}), []);
  assert.deepEqual(await run({latestId: 11}), []);
  assert.deepEqual(await run({latestEvent: 'schedule', latestId: 11}), []);
  assert.deepEqual(await run({path: '.github/workflows/other.yml'}), []);
  assert.deepEqual(await run({latestAttempt: 2}), []);
  assert.deepEqual(await run({latestStatus: 'in_progress'}), []);
  assert.equal((await run({conclusion: 'cancelled', issue}))[0][1].state, 'open');
  assert.equal((await run({conclusion: 'success', issue, jobs}))[0][1].state, 'open');
  assert.equal((await run({issue: {...issue, user: {login: 'someone-else'}}}))[0][0], 'create');
  await assert.rejects(run({apiError: true}), /API unavailable/);
  assert.deepEqual(await run({issue, conclusion: 'success'}), []);
  const changed = await run({issue: {...issue, state: 'open', body: 'Previous failure'}, jobs});
  assert.equal(changed[0][0], 'comment');
  assert.match(changed[0][1].body, /Test \(macos-14\)/);
  assert.equal(changed[1][0], 'update');
  assert.equal(changed[1][1].state, undefined);
  assert.deepEqual(await run({issue: {...issue, state: 'open', body: changed[1][1].body}, jobs}), []);
  console.log('Main CI health: 16 cases passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
