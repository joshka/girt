"""Original Git executable observations; no upstream implementation or fixtures used."""
import json
import os
import platform
import subprocess
import tempfile

env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
cases = []
for fmt in ['sha1', 'sha256']:
    with tempfile.TemporaryDirectory() as root:
        subprocess.run(['git', 'init', '--bare', '--quiet', '--object-format=' + fmt, root], env=env, check=True)
        for kind, payload in [('blob', b''), ('blob', b'hello\n'), ('blob', bytes(range(256))), ('tree', b''), ('commit', b'arbitrary\x00payload'), ('tag', b'arbitrary\xffpayload')]:
            result = subprocess.run(['git', '-C', root, 'hash-object', '--stdin', '--literally', '-t', kind], input=payload, env=env, capture_output=True, check=True)
            cases.append(dict(format=fmt, kind=kind, payload_hex=payload.hex(), oid=result.stdout.decode().strip()))
print(json.dumps(dict(git=subprocess.check_output(['git', '--version'], env=env).decode().strip(), platform=platform.platform(), cases=cases), indent=2))
