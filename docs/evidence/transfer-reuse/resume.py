import json,pathlib,statistics,re,subprocess
script=pathlib.Path('/tmp/girt-63-65-perf/run.py').read_text().split('records=[]')[0]
exec(script)
old=json.loads((root/'records.json').read_text())
root=pathlib.Path('/tmp/girt-63-65-final-measurements');root.mkdir(exist_ok=True)
records=[r for r in old if r['implementation']!='after']
for op in ['noop','increment','clone']:
 for round in range(3):
  implementations=['after'] if op!='clone' else (['main','after','git'] if round%2==0 else ['git','after','main'])
  for impl in implementations:
   source,dest=prepare(op,f'{round}-{impl}');log=root/f'{op}-{round}-{impl}.log'
   if impl=='git':cmd=['git','clone','--bare','--no-local',str(source),str(dest)]
   else:
    exe=f'/tmp/girt-63-65-perf{"-main" if impl=="main" else ""}/target/release/transfer-perf'
    cmd=[exe,'clone',str(source),str(dest)] if op=='clone' else [exe,'fetch',str(dest)]
   lock='until shlock -f /tmp/girt-timing.lock -p $$; do sleep 15; done\ntrap \'unlink /tmp/girt-timing.lock\' EXIT\n/usr/bin/time -l "$@"'
   with log.open('w') as out:res=subprocess.run(['sh','-c',lock,'measure',*cmd],stdout=out,stderr=out,env=env)
   data=log.read_text();print(op,round,impl,res.returncode,flush=True)
   if res.returncode:raise RuntimeError(data)
   records.append(dict(operation=op,implementation=impl,round=round,seconds=float(re.search(r'([0-9.]+) real',data).group(1)),rss=int(re.search(r'(\d+)\s+maximum resident set size',data).group(1)),log=str(log)))
   (root/'records.json').write_text(json.dumps(records,indent=2))
for op in ['noop','increment','clone']:
 for impl in ['main','after','git']:
  rs=[r for r in records if r['operation']==op and r['implementation']==impl];ts=[r['seconds'] for r in rs]
  print(op,impl,'median',statistics.median(ts),'range',min(ts),max(ts),'rss MiB',max(r['rss'] for r in rs)/1048576)
