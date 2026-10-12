import os,pathlib,subprocess,json,statistics
root=pathlib.Path('/tmp/girt-63-65-measurements');root.mkdir(exist_ok=True)
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
for op in ['noop','increment','clone']:
 for round in range(3):
  for impl in (['main','after','git'] if round%2==0 else ['git','after','main']):
   n=f'{round}-{impl}';source,dest=prepare(op,n);log=root/f'{op}-{n}.log'
   if impl=='git':cmd=['git','clone','--bare','--no-local',str(source),str(dest)] if op=='clone' else ['git','-C',str(dest),'fetch','origin']
   else:
    exe=f'/tmp/girt-63-65-perf{"-main" if impl=="main" else ""}/target/release/transfer-perf'
    cmd=[exe,'clone',str(source),str(dest)] if op=='clone' else [exe,'fetch',str(dest)]
   # Build is complete. Lock only the timed process, never fixture construction.
   script='until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done\ntrap \'unlink /tmp/girt-timing.lock\' EXIT\n/usr/bin/time -l "$@"'
   with log.open('w') as out:res=subprocess.run(['sh','-c',script,'measure',*cmd],stdout=out,stderr=out,env=env)
   text=log.read_text();print(op,n,res.returncode,flush=True)
   if res.returncode:raise RuntimeError(text)
   import re
   wall=float(re.search(r'([0-9.]+) real',text).group(1));rss=int(re.search(r'(\d+)\s+maximum resident set size',text).group(1))
   records.append(dict(operation=op,implementation=impl,round=round,seconds=wall,rss=rss,log=str(log)))
   (root/'records.json').write_text(json.dumps(records,indent=2))
for op in ['noop','increment','clone']:
 for impl in ['main','after','git']:
  rs=[r for r in records if r['operation']==op and r['implementation']==impl];ts=[r['seconds'] for r in rs]
  print(op,impl,'median',statistics.median(ts),'range',min(ts),max(ts),'rss MiB',max(r['rss'] for r in rs)/1048576)
