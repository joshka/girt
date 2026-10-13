"""macOS/APFS local-push comparison; setup is outside the shared timing lock.

Prepare fresh fixtures, then run under /tmp/girt-timing.lock with independently built
main/change drivers using the driver in docs/evidence/116-push-verify.md.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

ENV = dict(os.environ, GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')


def command(args, data=None, env=ENV):
    return subprocess.run(args, input=data, env=env, check=True, capture_output=True).stdout


def git(repo, *args, data=None, env=ENV):
    return command(['git', '-C', str(repo), *args], data, env).decode().strip()


def copy(source, destination):
    # Refuse reuse: an already-pushed bookmark invalidates a sample.
    assert not destination.exists(), destination
    command(['cp', '-cR', str(source), str(destination)])


def prepare(args):
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    env = dict(ENV, GIT_AUTHOR_NAME='bench', GIT_AUTHOR_EMAIL='bench@example.com',
               GIT_COMMITTER_NAME='bench', GIT_COMMITTER_EMAIL='bench@example.com',
               GIT_AUTHOR_DATE='1700000000 +0000', GIT_COMMITTER_DATE='1700000000 +0000')
    workloads = []
    for name, fixture, branch in [('git-git', args.git_git, 'master'),
                                  ('131k', args.fixture_131k, 'main')]:
        for depth in [400, 1000]:
            workload = f'{name}-{depth}'
            source = root / (workload + '-source.git')
            copy(fixture, source)
            # Remove all inherited remote configuration, including duplicate URLs.
            remotes = git(source, 'remote').splitlines()
            for remote in remotes:
                git(source, 'config', '--local', '--remove-section', f'remote.{remote}')
            base = git(source, 'rev-parse', f'{branch}~{depth}')
            tree = git(source, 'rev-parse', f'{base}^{{tree}}')
            tip = base
            for index in range(10):
                blob = git(source, 'hash-object', '-w', '--stdin',
                           data=f'125 {index}\n'.encode(), env=env)
                entries = command(['git', '-C', str(source), 'ls-tree', tree])
                entries += f'100644 blob {blob}\tbench125-{index}\n'.encode()
                tree = git(source, 'mktree', data=entries)
                tip = git(source, 'commit-tree', tree, '-p', tip,
                          data=f'125 commit {index}\n'.encode(), env=env)
            git(source, 'update-ref', 'refs/heads/stack', tip)
            workloads.append(dict(name=workload, fixture=str(fixture.resolve()),
                                  source=str(source), tip=tip, base=base))
    receiver = root / 'git-git-master-only.git'
    copy(args.git_git, receiver)
    refs = git(receiver, 'for-each-ref', '--format=%(refname)').splitlines()
    updates = ''.join(f'delete {ref}\n' for ref in refs if ref != 'refs/heads/master')
    git(receiver, 'update-ref', '--stdin', data=updates.encode())
    assert git(receiver, 'for-each-ref', '--format=%(refname)') == 'refs/heads/master'
    base_workload = next(item for item in workloads if item['name'] == 'git-git-1000')
    workloads.append(dict(base_workload, name='git-git-1000-master-only', fixture=str(receiver)))
    for workload in workloads:
        for round_number in range(3):
            for position in range(6):
                stem = f"{workload['name']}-{round_number}-{position}"
                destination = root / (stem + '-dest.git')
                source = root / (stem + '-source.git')
                copy(Path(workload['fixture']), destination)
                copy(Path(workload['source']), source)
                git(source, 'config', '--local', 'remote.origin.url', str(destination))
                assert git(source, 'config', '--get-all', 'remote.origin.url') == str(destination)
    (root / 'workloads.json').write_text(json.dumps(workloads, indent=2) + '\n')


def measure(args):
    root = args.output.resolve()
    results = []
    drivers = dict(main=args.main_driver.resolve(), change=args.change_driver.resolve())
    for workload in json.loads((root / 'workloads.json').read_text()):
        name = workload['name']
        if args.workload and name != args.workload:
            continue
        for round_number in range(3):
            for position, tool in enumerate(['git', 'main', 'change', 'change', 'main', 'git']):
                stem = f'{name}-{round_number}-{position}'
                source = root / (stem + '-source.git')
                destination = root / (stem + '-dest.git')
                assert git(source, 'config', '--get-all', 'remote.origin.url') == str(destination)
                exists = subprocess.run(['git', '-C', str(destination), 'show-ref', '--quiet', '--verify',
                                         'refs/heads/bench'], env=ENV, capture_output=True)
                assert exists.returncode == 1, exists.stderr
                if tool == 'git':
                    invocation = ['git', '-C', str(source), 'push', '--quiet', 'origin',
                                  'stack:refs/heads/bench']
                else:
                    invocation = [str(drivers[tool]), str(source), workload['tip']]
                start = time.perf_counter()
                process = subprocess.run(['/usr/bin/time', '-l', *invocation], env=ENV,
                                         capture_output=True)
                elapsed = time.perf_counter() - start
                stderr = process.stderr.decode()
                (root / (stem + '.stderr')).write_text(stderr)
                assert process.returncode == 0, stderr
                rss = int(re.search(r'(\d+)\s+maximum resident set size', stderr)[1])
                assert git(destination, 'rev-parse', 'refs/heads/bench') == workload['tip']
                result = dict(repo=name, round=round_number, position=position, tool=tool,
                              seconds=elapsed, rss_mib=rss / 1048576, load=os.getloadavg())
                results.append(result)
                (root / args.results).write_text(json.dumps(results, indent=2) + '\n')
                print(result, flush=True)


parser = argparse.ArgumentParser(description=__doc__)
subparsers = parser.add_subparsers(dest='stage', required=True)
setup = subparsers.add_parser('prepare')
setup.add_argument('--output', type=Path, required=True)
setup.add_argument('--git-git', type=Path, required=True)
setup.add_argument('--fixture-131k', type=Path, required=True)
setup.set_defaults(action=prepare)
run = subparsers.add_parser('run')
run.add_argument('--output', type=Path, required=True)
run.add_argument('--main-driver', type=Path, required=True)
run.add_argument('--change-driver', type=Path, required=True)
run.add_argument('--workload')
run.add_argument('--results', default='results.json')
run.set_defaults(action=measure)
arguments = parser.parse_args()
arguments.action(arguments)
