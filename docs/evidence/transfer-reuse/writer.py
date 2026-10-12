import json,pathlib,re,subprocess,os,statistics
root=pathlib.Path('/tmp/girt-63-65-writer-measurements');root.mkdir(exist_ok=True)
env=os.environ.copy();env.update(GIT_CONFIG_GLOBAL='/dev/null',GIT_CONFIG_NOSYSTEM='1')
records=[]
for round in range(3):
 for impl in (['main','after','git'] if round%2==0 else ['git','after','main']):
  source=root/f'{round}-{impl}.git';subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(source)],check=True)
  if impl=='git':
   cmd=['git','--git-dir='+str(source),'pack-objects','--all','--stdout'];output=subprocess.DEVNULL
  else:
   exe=f'/tmp/girt-63-65-perf{"-main" if impl=="main" else ""}/target/release/transfer-perf'
   cmd=[exe,'prepare-transfer',str(source)];output=None
  log=root/f'{round}-{impl}.log'
  lock='until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done\ntrap \'unlink /tmp/girt-timing.lock\' EXIT\n/usr/bin/time -l "$@"'
  with log.open('w') as err:
   done=subprocess.run(['sh','-c',lock,'measure',*cmd],stdout=err if output is None else output,stderr=err,env=env)
  text=log.read_text();print(round,impl,done.returncode,flush=True)
  if done.returncode:raise RuntimeError(text)
  phases=dict((k,float(v)) for k,v in re.findall(r'phase ([\w-]+)=([\d.]+)',text))
  records.append(dict(round=round,implementation=impl,seconds=float(re.search(r'([\d.]+) real',text).group(1)),rss=int(re.search(r'(\d+)\s+maximum resident set size',text).group(1)),phases=phases,log=str(log)))
  (root/'records.json').write_text(json.dumps(records,indent=2))
for impl in ['main','after','git']:
 rs=[r for r in records if r['implementation']==impl];ts=[r['seconds'] for r in rs]
 print(impl,statistics.median(ts),min(ts),max(ts),max(r['rss'] for r in rs)/1048576)
 if impl!='git':print('writer',impl,statistics.median([r['phases']['write'] for r in rs]))
