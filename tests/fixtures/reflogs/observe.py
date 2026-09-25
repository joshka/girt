"""Original executable-only probes of R11/R14 operations, never upstream source fixtures."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

ENV = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
ENV.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
           GIT_AUTHOR_NAME="A", GIT_AUTHOR_EMAIL="a@b", GIT_COMMITTER_NAME="A",
           GIT_COMMITTER_EMAIL="a@b", GIT_AUTHOR_DATE="@1700000000 +0000",
           GIT_COMMITTER_DATE="@1700000000 +0000")
CASES = {
    "r11_short": b"A <a@b> 1 +0\tfixture\n",
    "r14_short": b"A <a@b> 1 +01\tfixture\n",
    "short_long_date": b"A <a@b> 1700000000 +01\tfixture\n",
    "r11_suffix": b"A <a@b> 1 +0000junk\tfixture\n",
    "r14_suffix": b"A <a@b> 1 +0000suffix\tfixture\n",
    "overflow": b"A <a@b> 9223372036854775808 +0000\tfixture\n",
    "r11_unterminated": b"A <a@b> 1 +0000\tmessage",
    "r14_unterminated": b"A <a@b> 1 +0000\tfixture",
    "unterminated_long_date": b"A <a@b> 1700000000 +0000\tfixture",
    "r11_cr": b"A <a@b> 1 +0000\tmessage\r\n",
    "r14_cr": b"A <a@b> 1 +0000\ta\rb\n",
    "r11_nul": b"A <a@b> 1 +0000\tx\0y\n",
    "r14_nul": b"A <a@b> 1 +0000\ta\0b\n",
}
observations = []
for fmt in ("sha1", "sha256"):
    with tempfile.TemporaryDirectory() as directory:
        def git(*args, data=None):
            return subprocess.run(["git", *args], cwd=directory, env=ENV, input=data,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert git("init", "--initial-branch=main", "--object-format=" + fmt).returncode == 0
        tree = git("mktree", data=b"").stdout.strip().decode()
        tip = git("commit-tree", tree, data=b"original\n").stdout.strip()
        assert git("update-ref", "HEAD", tip.decode()).returncode == 0
        head = Path(directory) / ".git/HEAD"
        log = Path(directory) / ".git/logs/HEAD"
        for case, tail in CASES.items():
            raw = b"0" * len(tip) + b" " + tip + b" " + tail
            for mode in ("symbolic", "detached"):
                head.write_bytes(b"ref: refs/heads/main\n" if mode == "symbolic" else tip + b"\n")
                log.write_bytes(raw)
                for args in (("reflog", "show", "--format=%H %gn %gs", "HEAD"),
                             ("rev-list", "--reflog", "--all")):
                    result = git(*args)
                    observations.append(dict(format=fmt, case=case, head=mode,
                        input_hex=raw.hex(), operation=list(args), status=result.returncode,
                        stdout_hex=result.stdout.hex(), stderr_hex=result.stderr.hex()))
            before = log.read_bytes()
            result = git("update-ref", "--no-deref", "-m", "append", "HEAD", tip.decode())
            after = log.read_bytes()
            observations.append(dict(format=fmt, case=case, operation=["update-ref", "--no-deref", "-m", "append", "HEAD", tip.decode()],
                status=result.returncode, preserved_prefix=after.startswith(before),
                appended_hex=after[len(before):].hex(), input_sha256=hashlib.sha256(before).hexdigest()))
Path("reflog-observations.json").write_text(json.dumps(observations, indent=2) + "\n")
print(f"Retained {len(observations)} Git operation observations")
