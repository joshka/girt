import subprocess, pathlib, json, re
root=pathlib.Path('/tmp/girt-25-evidence')
def run(args, **kw): return subprocess.run(args,check=True,**kw)
records=[]
for n in range(3):
 for op in ['new','ff']:
  for impl in ['girt','git']:
   p=root/f'{op}-{impl}-{n}'
   src=p.with_suffix('.git'); dst=p.with_suffix('.dest.git')
   run(['cp','-cR','/tmp/opus-med-23/source.git',str(src)])
   run(['cp','-cR','/tmp/opus-med-23/source.git',str(dst)])
   run(['cp','-cR','/tmp/perf-audit/pushsrc.git/objects/.',str(src/'objects')])
   old='987c084b8f3ff28e8246d8b38c55c323c85611d5'; new='812a3cdbfb334543bbd5858335e21fc5c941287b'
   run(['git','-C',str(src),'config','remote.origin.url',str(dst)])
   run(['git','-C',str(src),'update-ref','refs/heads/bench-push',new])
   if op=='ff': run(['git','-C',str(dst),'update-ref','refs/heads/bench-push',old])
   cmd=['/tmp/girt-25-driver/target/release/perfdrv','push',str(src),new,old if op=='ff' else '-'] if impl=='girt' else ['git','-C',str(src),'push','-q','origin','bench-push']
   with open(str(p)+'.log','w') as log: run(['/usr/bin/time','-l',*cmd],stdout=log,stderr=log)
   text=pathlib.Path(str(p)+'.log').read_text(); secs=float(re.search(r'([\d.]+) real',text).group(1)); rss=int(re.search(r'(\d+)  maximum resident set size',text).group(1))
   records.append(dict(op=op,impl=impl,n=n,seconds=secs,rss=rss)); print(records[-1],flush=True)
   run(['git','-C',str(dst),'fsck','--connectivity-only'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
(root/'baseline.json').write_text(json.dumps(records,indent=2))
