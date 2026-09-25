"""Original Git CLI fixtures; inspect no upstream implementation or test sources."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(args, cwd, env, data=None):
    result = subprocess.run(args, cwd=cwd, env=env, input=data, capture_output=True, timeout=45)
    return {"exit": result.returncode, "stdout": result.stdout.decode(errors="backslashreplace"),
            "stderr": result.stderr.decode(errors="backslashreplace")}


def checked(args, cwd, env):
    result = run(args, cwd, env)
    assert result["exit"] == 0, (args, result)
    return result["stdout"].strip()


def observe(root, name, fmt, mutate, env):
    base = root / (fmt + "-" + name)
    base.mkdir()
    repo = base / "repo"
    checked(["git", "init", "-q", "--template=", "--initial-branch=main",
             "--object-format=" + fmt, str(repo)], base, env)
    (repo / "in").mkdir()
    (repo / "out").mkdir()
    (repo / "in/file").write_text("original external storage fixture\n")
    (repo / "out/file").write_text("outside cone\n")
    checked(["git", "add", "."], repo, env)
    checked(["git", "-c", "commit.gpgsign=false", "commit", "-qm", "external fixture"], repo, env)
    tip = checked(["git", "rev-parse", "HEAD"], repo, env)
    mutate(repo, base, fmt, tip, env)
    git = run(["git", "cat-file", "-p", tip], repo, env)
    reflog_read = run(["git", "reflog", "show", "--format=%H %gn %gs", "HEAD"], repo, env)
    reflog_read["stdout_bytes"] = len(reflog_read["stdout"].encode())
    if len(reflog_read["stdout"]) > 2048:
        reflog_read["stdout"] = reflog_read["stdout"][:2048] + "<truncated>"
    index_before = run(["git", "ls-files", "--sparse", "--stage"], repo, env)
    init = run(["jj", "--config", "signing.behavior='drop'", "git", "init", "--colocate"], repo, env)
    log = run(["jj", "--ignore-working-copy", "log", "--no-graph", "-r", "all()", "-T", "description"], repo, env) if init["exit"] == 0 else None
    status = run(["jj", "status"], repo, env) if init["exit"] == 0 else None
    return {"format": fmt, "case": name, "git": git, "jj_init": init, "jj_log": log,
            "git_reflog": reflog_read, "index_before": index_before,
            "index_after": run(["git", "ls-files", "--sparse", "--stage"], repo, env),
            "imported": bool(log and "external fixture" in log["stdout"]), "jj_status": status}


def alternate(kind):
    def mutate(repo, base, fmt, tip, env):
        objects = repo / ".git/objects"
        donor = base / "donor"
        shutil.copytree(objects, donor)
        for node in objects.iterdir():
            if node.name not in ("info", "pack"):
                shutil.rmtree(node)
        target = str(donor)
        if kind == "relative":
            target = "../../../donor"
        if kind == "cycle":
            (donor / "info/alternates").write_text(str(objects) + "\n")
        if kind == "missing":
            target = str(base / "absent")
        if kind == "missing_then_valid":
            target = str(base / "absent") + "\n" + str(donor)
        if kind == "symlink":
            (base / "alias").symlink_to(donor, target_is_directory=True)
            target = str(base / "alias")
        (objects / "info/alternates").write_text(target + "\n")
    return mutate


def packed(version, index):
    def mutate(repo, base, fmt, tip, env):
        checked(["git", "repack", "-ad"], repo, env)
        pack = next((repo / ".git/objects/pack").glob("*.pack"))
        idx = pack.with_suffix(".idx")
        idx.unlink()
        raw = pack.read_bytes()
        width = 20 if fmt == "sha1" else 32
        raw = raw[:4] + version.to_bytes(4, "big") + raw[8:-width]
        pack.chmod(0o644)
        pack.write_bytes(raw + hashlib.new(fmt, raw).digest())
        checked(["git", "index-pack", "--index-version=" + str(index), str(pack)], repo, env)
    return mutate


def auxiliary(repo, base, fmt, tip, env):
    checked(["git", "repack", "-adb"], repo, env)
    checked(["git", "multi-pack-index", "write"], repo, env)
    checked(["git", "commit-graph", "write", "--reachable"], repo, env)


def storage_link(repo, base, fmt, tip, env):
    path = repo / ".git/objects"
    path.rename(base / "objects")
    path.symlink_to(base / "objects", target_is_directory=True)


def extension(key, value):
    def mutate(repo, base, fmt, tip, env):
        checked(["git", "config", "core.repositoryformatversion", "1"], repo, env)
        checked(["git", "config", "extensions." + key, value], repo, env)
    return mutate


def sidecar(name):
    def mutate(repo, base, fmt, tip, env):
        (repo / ".git/objects/info" / name).write_text("original deliberately invalid auxiliary bytes\n")
    return mutate


def intent(repo, base, fmt, tip, env):
    (repo / "new").write_text("intent fixture\n")
    checked(["git", "add", "--intent-to-add", "new"], repo, env)


def command(*args):
    return lambda repo, base, fmt, tip, env: checked(["git", *args], repo, env)


def reflog(kind):
    def mutate(repo, base, fmt, tip, env):
        identity = {"short_zone": "A <a@b> 1 +01", "suffix_zone": "A <a@b> 1 +0000suffix",
                    "overflow": "A <a@b> 9223372036854775808 +0000"}.get(kind, "A <a@b> 1 +0000")
        message = {"cr": b"a\rb", "nul": b"a\x00b", "large": b"x" * 1048576}.get(kind, b"fixture")
        line = ("0" * len(tip) + " " + tip + " " + identity + "\t").encode() + message
        if kind != "unterminated":
            line += b"\n"
        (repo / ".git/logs/HEAD").write_bytes(line)
    return mutate


with tempfile.TemporaryDirectory(prefix="girt-r14-") as tmp:
    root = Path(tmp)
    (root / "home").mkdir()
    (root / "jj.toml").write_text('[user]\nname="Fixture"\nemail="fixture@example.com"\n')
    env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "JJ_"))}
    env.update(HOME=str(root / "home"), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(root / "absent"),
               GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.com",
               GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.com",
               GIT_AUTHOR_DATE="@1700000000 +0000", GIT_COMMITTER_DATE="@1700000000 +0000",
               JJ_CONFIG=str(root / "jj.toml"), JJ_PAGER="cat")
    cases = [("control", lambda *args: None), ("objects_symlink", storage_link)]
    cases += [("alternate_" + kind, alternate(kind)) for kind in
              ("absolute", "relative", "cycle", "missing", "missing_then_valid", "symlink")]
    cases += [(f"pack{v}_index{i}", packed(v, i)) for v, i in ((2, 1), (2, 2), (3, 1), (3, 2))]
    cases += [("split", command("update-index", "--split-index")),
              ("sparse", command("sparse-checkout", "set", "--cone", "--sparse-index", "in")),
              ("reftable", command("refs", "migrate", "--ref-format=reftable"))]
    cases += [("intent", intent), ("skip", command("update-index", "--skip-worktree", "out/file")),
              ("unknown_extension", extension("girtUnknown", "true")),
              ("noop_extension", extension("noop", "true")),
              ("partial_clone", extension("partialClone", "origin")),
              ("precious", extension("preciousObjects", "true")),
              ("worktree_config", extension("worktreeConfig", "true")),
              ("http_alternates", sidecar("http-alternates")),
              ("commit_graph", command("commit-graph", "write", "--reachable")),
              ("auxiliary_indexes", auxiliary)]
    cases += [("reflog_" + k, reflog(k)) for k in
              ("short_zone", "suffix_zone", "overflow", "cr", "nul", "large", "unterminated")]
    rows = [observe(root, name, fmt, fn, env) for fmt in ("sha1", "sha256") for name, fn in cases]
    result = {"git": checked(["git", "--version"], root, env),
              "jj": checked(["jj", "--version"], root, env), "cases": rows}
    print(json.dumps(result, indent=2).replace(tmp, "<fixture>"))
