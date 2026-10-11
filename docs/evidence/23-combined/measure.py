import json, re, subprocess, hashlib
from pathlib import Path
r=Path('/tmp/23-combined-bench');records=[]
labels=['combined','opus-low','sol-medium','git']
for round_ in range(3):
 order=labels[round_:]+labels[:round_]
 for mode in ['repack','maint']:
  for slot,label in enumerate(order):
   name=f'{mode}-{round_+1}-{slot}-{label}';dest=r/name
   subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(dest)],check=True)
   if label=='git':
    command=['git','--git-dir='+str(dest),*(['repack','-ad'] if mode=='repack' else ['gc','--prune=now'])]
   else: command=[str(r/'target/release'/('bench-'+label)),mode,str(dest)]
   with (r/(name+'.out')).open('w') as out,(r/(name+'.time')).open('w') as err:
    code=subprocess.run(['/usr/bin/time','-l',*command],stdout=out,stderr=err).returncode
   timing=(r/(name+'.time')).read_text();output=(r/(name+'.out')).read_text()
   real=re.search(r'([\d.]+) real\s',timing);rss=re.search(r'(\d+)\s+maximum resident set size',timing)
   packs={p.name:p.stat().st_size for p in (dest/'objects/pack').glob('*.pack')}
   record=dict(label=label,mode=mode,round=round_+1,slot=slot,status=code,seconds=float(real[1]) if real else None,rss_bytes=int(rss[1]) if rss else None,packs=packs,total_pack_bytes=sum(packs.values()),path=str(dest))
   records.append(record);(r/'records.json').write_text(json.dumps(records,indent=2))
   print(json.dumps({k:v for k,v in record.items() if k not in ['packs','path']}),flush=True)
   if code: raise RuntimeError(name+' failed')
   if round_!=0: subprocess.run(['rm','-rf',str(dest)],check=True)

for round_ in range(1,4):
 dest=r/f'stages-{round_}'
 subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(dest)],check=True)
 with (r/f'stages-{round_}.out').open('w') as out,(r/f'stages-{round_}.time').open('w') as err:
  subprocess.run(['/usr/bin/time','-l',str(r/'target/release/stages'),str(dest)],stdout=out,stderr=err,check=True)
 print((r/f'stages-{round_}.out').read_text(),flush=True)
 subprocess.run(['rm','-rf',str(dest)],check=True)
