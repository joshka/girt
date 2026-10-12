import os,pathlib,subprocess,json,statistics
root=pathlib.Path('/tmp/girt-72-final-measurements');root.mkdir(exist_ok=True)
base=pathlib.Path('/tmp/opus-med-23/source.git')
env=os.environ.copy();env.update(GIT_CONFIG_GLOBAL='/dev/null',GIT_CONFIG_NOSYSTEM='1',GIT_AUTHOR_NAME='Perf',GIT_AUTHOR_EMAIL='perf@example.invalid',GIT_COMMITTER_NAME='Perf',GIT_COMMITTER_EMAIL='perf@example.invalid')
def git(path,*args,input=None):return subprocess.check_output(['git','-C',str(path),*args],input=input,env=env)
def prepare(op,n):
 d=root/f'{op}-{n}';d.mkdir();s=d/'source.git';t=d/'dest.git'
 subprocess.run(['cp','-cR',str(base),str(s)],check=True)
 if op!='clone':
  subprocess.run(['cp','-cR',str(base),str(t)],check=True)
  git(t,'config','remote.origin.url',str(s));git(t,'config','remote.origin.fetch','+refs/heads/*:refs/remotes/origin/*')
  lines=git(t,'for-each-ref','--format=%(objectname) %(refname)','refs/heads').decode().splitlines()
  for line in lines:
   oid,ref=line.split();git(t,'update-ref',ref.replace('refs/heads/','refs/remotes/origin/'),oid)
 if op=='increment':
  old=git(s,'rev-parse','HEAD').strip();blob=git(s,'hash-object','-w','--stdin',input=b'one changed file for transfer benchmark\n').strip()
  entries=git(s,'ls-tree','HEAD').splitlines();entries=[e for e in entries if not e.endswith(b'\tREADME.md')];entries.append(b'100644 blob '+blob+b'\tREADME.md')
  tree=git(s,'mktree',input=b'\n'.join(entries)+b'\n').strip();commit=git(s,'commit-tree',tree.decode(),'-p',old.decode(),input=b'transfer benchmark\n').strip();git(s,'update-ref','HEAD',commit.decode())
 return s,t
records=[]
import re
for op in ['noop','increment']:
 for round in range(3):
  source,dest=prepare(op,f'{round}-after');log=root/f'{op}-{round}-after.log'
  cmd=['/tmp/girt-63-65-perf/target/release/transfer-perf','fetch',str(dest)]
  lock='until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done\ntrap \'unlink /tmp/girt-timing.lock\' EXIT\n/usr/bin/time -l "$@"'
  with log.open('w') as out:done=subprocess.run(['sh','-c',lock,'measure',*cmd],stdout=out,stderr=out,env=env)
  data=log.read_text();print(op,round,done.returncode,flush=True)
  if done.returncode:raise RuntimeError(data)
  records.append(dict(operation=op,implementation='after',round=round,seconds=float(re.search(r'([0-9.]+) real',data).group(1)),rss=int(re.search(r'(\d+)\s+maximum resident set size',data).group(1)),log=str(log)))
  (root/'records.json').write_text(json.dumps(records,indent=2))
for op in ['noop','increment']:
 rows=[r for r in records if r['operation']==op];ts=[r['seconds'] for r in rows]
 print(op,statistics.median(ts),min(ts),max(ts),max(r['rss'] for r in rows)/1048576)
