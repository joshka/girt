"""Original Git CLI probes; no implementation or upstream fixtures are consulted."""
import json
import os
from pathlib import Path
import subprocess
import tempfile


def run(path, args, data=b""):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(path / "absent"))
    result = subprocess.run(["git", "-C", str(path), *args], input=data,
                            capture_output=True, env=env, check=False)
    return {"code": result.returncode, "stdout": result.stdout.decode(errors="backslashreplace"),
            "stderr": result.stderr.decode(errors="backslashreplace")}


records = []
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary)
    for fmt in ("sha1", "sha256"):
        a, b = root / (fmt + "-a"), root / (fmt + "-b")
        a.mkdir()
        b.mkdir()
        for path in (a, b):
            assert run(path, ["init", "--bare", "--template=", "--object-format=" + fmt])["code"] == 0
        oid = run(b, ["hash-object", "-w", "--stdin"], b"original R39 probe")["stdout"].strip()
        relative = ("../../" + b.name + "/objects").encode()
        cases = {"relative": relative + b"\n", "quoted": b'"' + relative + b'"\n',
                 "CR": relative + b"\r\n", "empty-comment": b"\n#ignored\n" + relative + b"\n",
                 "quote-suffix": b'"' + relative + b'"ignored\n',
                 "NUL": relative + b"\0ignored\n", "bad-escape": b'"bad\\q"\n' + relative + b"\n"}
        for case, data in cases.items():
            (a / "objects/info/alternates").write_bytes(data)
            records.append({"format": fmt, "case": case, "bytes": data.hex(),
                            **run(a, ["cat-file", "blob", oid])})
        (a / "objects/info/alternates").unlink()
        for name in ("noop", "preciousObjects", "partialClone"):
            for value in ("true", "nonsense", "", None):
                setting = name + ("=" + value if value is not None else "")
                object_format = "objectformat=" + fmt + "\n"
                (a / "config").write_text("[core]\nbare=true\nrepositoryformatversion=1\n"
                                          "[extensions]\n" + object_format + setting + "\n")
                records.append({"format": fmt, "extension": name, "value": value,
                                **run(a, ["rev-parse", "--git-dir"])})
print(json.dumps({"git": subprocess.check_output(["git", "--version"], text=True).strip(),
                  "observations": records}, indent=2))
