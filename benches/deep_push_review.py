"""Prepare/measure git/git review cases outside/inside the shared timing lock.

One sample per Git, pre-change main and change; requires the driver from the linked
issue-116 evidence. This is supplementary evidence, not a statistical parity claim.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time

ENV = dict(os.environ, GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
           GIT_AUTHOR_NAME='bench', GIT_AUTHOR_EMAIL='bench@example.com',
           GIT_COMMITTER_NAME='bench', GIT_COMMITTER_EMAIL='bench@example.com',
           GIT_AUTHOR_DATE='1700000000 +0000', GIT_COMMITTER_DATE='1700000000 +0000')


def command(args, data=None):
    return subprocess.run(args, input=data, env=ENV, check=True, capture_output=True).stdout


def git(repo, *args, data=None):
    return command(['git', '-C', str(repo), *args], data).decode().strip()


def copy(source, destination):
    assert not destination.exists(), destination
    command(['cp', '-cR', str(source), str(destination)])


def prepare(args):
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    workloads = []
    for name, depth in [('unreachable-base', 1000), ('50000-refs', 1)]:
        receiver = root / (name + '-receiver.git')
        copy(args.fixture, receiver)
        if name == '50000-refs':
            master = git(receiver, 'rev-parse', 'master')
            refs = git(receiver, 'for-each-ref', '--format=%(refname)').splitlines()
            updates = ''.join(f'delete {ref}\n' for ref in refs if ref != 'refs/heads/master')
            git(receiver, 'update-ref', '--stdin', data=updates.encode())
            stream = ''.join(
                f'commit refs/heads/r{index:05}\ncommitter B <b@example.com> 1700000000 +0000\n'
                f'data {len(message)}\n{message}from {master}\n'
                for index in range(49999)
                for message in [f'bookmark {index}\n']
            )
            git(receiver, 'fast-import', '--quiet', data=stream.encode())
            git(receiver, 'pack-refs', '--all', '--prune')
            assert len(git(receiver, 'for-each-ref', '--format=%(refname)').splitlines()) == 50000
            assert len(set(git(receiver, 'for-each-ref', '--format=%(objectname)').splitlines())) == 50000
        source = root / (name + '-source.git')
        copy(receiver, source)
        for remote in git(source, 'remote').splitlines():
            git(source, 'config', '--local', '--remove-section', f'remote.{remote}')
        base = git(source, 'rev-parse', f'master~{depth}')
        tree = git(source, 'rev-parse', f'{base}^{{tree}}')
        if name == 'unreachable-base':
            base = git(source, 'commit-tree', tree, '-p', base, data=b'formerly published bookmark\n')
            payload = command(['git', '-C', str(source), 'cat-file', 'commit', base])
            assert git(receiver, 'hash-object', '-w', '-t', 'commit', '--stdin', data=payload) == base
            git(receiver, 'update-ref', 'refs/heads/retired-bookmark', base)
            git(receiver, 'update-ref', 'refs/heads/retired-bookmark', git(receiver, 'rev-parse', 'master'))
            reachable = git(receiver, 'rev-list', '--all').splitlines()
            assert base not in reachable
            assert git(receiver, 'cat-file', '-t', base) == 'commit'
        tip = base
        for index in range(10):
            blob = git(source, 'hash-object', '-w', '--stdin', data=f'review {index}\n'.encode())
            entries = command(['git', '-C', str(source), 'ls-tree', tree])
            entries += f'100644 blob {blob}\tbench125-review-{index}\n'.encode()
            tree = git(source, 'mktree', data=entries)
            tip = git(source, 'commit-tree', tree, '-p', tip, data=f'review stack {index}\n'.encode())
        git(source, 'update-ref', 'refs/heads/stack', tip)
        workload = dict(name=name, base=base, tip=tip, receiver=str(receiver))
        workloads.append(workload)
        for tool in ['git', 'main', 'change']:
            destination = root / f'{name}-{tool}-dest.git'
            sender = root / f'{name}-{tool}-source.git'
            copy(receiver, destination)
            copy(source, sender)
            git(sender, 'config', 'remote.origin.url', str(destination))
    (root / 'workloads.json').write_text(json.dumps(workloads, indent=2) + '\n')


def measure(args):
    root = args.output.resolve()
    drivers = dict(main=args.main_driver.resolve(), change=args.change_driver.resolve())
    results = []
    for workload in json.loads((root / 'workloads.json').read_text()):
        name = workload['name']
        for tool in ['git', 'main', 'change']:
            destination = root / f'{name}-{tool}-dest.git'
            sender = root / f'{name}-{tool}-source.git'
            assert git(sender, 'config', '--get-all', 'remote.origin.url') == str(destination)
            absent = subprocess.run(['git', '-C', str(destination), 'show-ref', '--quiet',
                                     '--verify', 'refs/heads/bench'], env=ENV, capture_output=True)
            assert absent.returncode == 1
            if tool == 'git':
                invocation = ['git', '-C', str(sender), 'push', '--quiet', 'origin', 'stack:refs/heads/bench']
            else:
                invocation = [str(drivers[tool]), str(sender), workload['tip']]
            started = time.perf_counter()
            process = subprocess.run(['/usr/bin/time', '-l', *invocation], env=ENV, capture_output=True)
            elapsed = time.perf_counter() - started
            stderr = process.stderr.decode()
            (root / f'{name}-{tool}.stderr').write_text(stderr)
            assert process.returncode == 0, stderr
            assert git(destination, 'rev-parse', 'refs/heads/bench') == workload['tip']
            rss = int(re.search(r'(\d+)\s+maximum resident set size', stderr)[1]) / 1048576
            result = dict(repo=name, tool=tool, seconds=elapsed, rss_mib=rss, load=os.getloadavg())
            results.append(result)
            (root / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
            print(result, flush=True)


parser = argparse.ArgumentParser(description=__doc__)
subparsers = parser.add_subparsers(required=True)
setup = subparsers.add_parser('prepare')
setup.add_argument('--output', type=Path, required=True)
setup.add_argument('--fixture', type=Path, required=True)
setup.set_defaults(action=prepare)
run = subparsers.add_parser('run')
run.add_argument('--output', type=Path, required=True)
run.add_argument('--main-driver', type=Path, required=True)
run.add_argument('--change-driver', type=Path, required=True)
run.set_defaults(action=measure)
args = parser.parse_args()
args.action(args)
