"""Original R01 fixtures: observe Git executables, never implementation/test source."""
import json
import os
import platform
from pathlib import Path
import sys
import subprocess
import tempfile


def observe():
    with tempfile.TemporaryDirectory() as root:
        directory = root
        env = {k: v for k, v in os.environ.items() if not k.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=directory + '/absent')

        def git(*args, data=b''):
            result = subprocess.run(['git', *args], input=data, cwd=directory,
                                    env=env, capture_output=True, check=False)
            return dict(code=result.returncode, out=result.stdout.hex(), err=result.stderr.hex())

        cases = {
            'negative': b'A <a> -1 +0000',
            'min': b'A <a> -9223372036854775808 -2359',
            'max': b'A <a> 9223372036854775807 +2359',
            'overflow': b'A <a> 9223372036854775808 +0000',
            'malformed': b'A <a> now +0000',
            'missing_date': b'A <a>',
            'zone_hours': b'A <a> 1 +2400',
            'zone_minutes': b'A <a> 1 -0060',
            'zone_short': b'A <a> 1 +000',
            'lexical': b'A <a> +00042 -0000',
            'double_space': b'A <a>  1 +0000',
            'tab': b'A <a>\t1 +0000',
            'no_space': b'A<a> 1 +0000',
            'overlap': b'A <B <a> 1 +0000',
            'closing': b'A > B <a> 1 +0000',
            'empty': b' <> 1 +0000',
            'binary': b' A\xff \t<a\xfe> 1 +0000',
        }
        records = {}
        for name, person in cases.items():
            directory = str(Path(root) / name)
            Path(directory).mkdir()
            git('init', '--bare', '--template=', '--object-format=sha1', '.')
            tree = bytes.fromhex(git('mktree')['out']).strip()
            payload = b'tree ' + tree + b'\nauthor ' + person + b'\ncommitter C <c> 1 +0000\n\nbody\n'
            stored = git('hash-object', '-w', '--literally', '-t', 'commit', '--stdin', data=payload)
            oid = bytes.fromhex(stored['out']).strip().decode()
            records[name] = dict(payload=payload.hex(), oid=oid,
                                 cat=git('cat-file', 'commit', oid),
                                 show=git('show', '-s', '--format=%an%x00%ae%x00%at%x00%ai', oid),
                                 fsck=git('fsck', '--strict', '--no-reflogs', oid))
        capture = Path(root) / 'capture'
        capture.mkdir()
        probe = Path(root) / 'gpg-probe'
        probe.write_text('#!' + sys.executable + '\n' +
                         'import pathlib, sys\n' +
                         'root = pathlib.Path(' + repr(str(capture)) + ')\n' +
                         '(root / "payload").write_bytes(sys.stdin.buffer.read())\n' +
                         '(root / "signature").write_bytes(pathlib.Path(sys.argv[-2]).read_bytes())\n' +
                         'sys.exit(1)\n')
        probe.chmod(0o700)
        signatures = {}
        for name, headers in {
            'single': b'gpgsig -----BEGIN PGP SIGNATURE-----\n first\n',
            'repeated': b'gpgsig -----BEGIN PGP SIGNATURE-----\n first\ngpgsig -----BEGIN PGP SIGNATURE-----\n second\n',
            'both': b'gpgsig -----BEGIN PGP SIGNATURE-----\n first\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n second\n',
            'sha256_only': b'gpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n second\n',
        }.items():
            for old in capture.iterdir():
                old.unlink()
            payload = b'tree ' + tree + b'\nauthor A <a> 1 +0000\ncommitter C <c> 1 +0000\n' + headers + b'\nbody\n'
            stored = git('hash-object', '-w', '--literally', '-t', 'commit', '--stdin', data=payload)
            oid = bytes.fromhex(stored['out']).strip().decode()
            result = git('-c', 'gpg.program=' + str(probe), 'verify-commit', oid)
            signatures[name] = dict(payload=payload.hex(), verify=result,
                                    captured={p.name: p.read_bytes().hex() for p in capture.iterdir()})
        return dict(platform=platform.platform(), git=git('--version'), cases=records,
                    signature_probes=signatures)


if __name__ == '__main__':
    print(json.dumps(observe(), indent=2))
