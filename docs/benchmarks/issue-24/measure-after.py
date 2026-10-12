from pathlib import Path
import subprocess, json, re, sys, shutil, os
root = Path('target/issue24').resolve()
runs = root / 'guarded-runs'
runs.mkdir(exist_ok=True)
os.environ['GIT_CONFIG_GLOBAL'] = '/dev/null'
os.environ['GIT_CONFIG_NOSYSTEM'] = '1'
order = [('after', mode) for _ in range(3) for mode in ('gc', 'repack-old', 'maintenance', 'repack', 'plan')]
records = []
for index, (variant, mode) in enumerate(order):
    fixture = runs / f'{index:02}-{variant}-{mode}.git'
    subprocess.run(['cp', '-cR', '/tmp/opus-med-23/source.git', str(fixture)], check=True)
    if variant == 'git':
        command = ['git', '-C', str(fixture), 'gc', '--prune=now'] if mode == 'gc' else ['git', '-C', str(fixture), 'repack', '-ad']
    else:
        command = [str(root / f'build-{variant}/release/gc-profile-{variant}'), mode, str(fixture)]
    script = "until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done\ntrap 'rm -f /tmp/girt-timing.lock' EXIT\n/usr/bin/time -l \"$@\""
    print(f'Start {index}: {variant} {mode}', flush=True)
    result = subprocess.run(['bash', '-c', script, 'measure', *command], capture_output=True, text=True)
    (root / f'guarded-{index:02}-{variant}-{mode}.out').write_text(result.stdout)
    (root / f'guarded-{index:02}-{variant}-{mode}.err').write_text(result.stderr)
    if result.returncode:
        print(result.stderr, flush=True)
        sys.exit(result.returncode)
    wall = float(re.search(r'([0-9.]+) real', result.stderr).group(1))
    rss = int(re.search(r'(\d+)\s+maximum resident set size', result.stderr).group(1))
    record = {'variant': variant, 'mode': mode, 'wall': wall, 'rss': rss}
    records.append(record)
    (root / 'guarded-timings.json').write_text(json.dumps(records, indent=2))
    print(record, flush=True)
    subprocess.run(['git', '-C', str(fixture), 'fsck', '--connectivity-only'], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True)
    shutil.rmtree(fixture)
