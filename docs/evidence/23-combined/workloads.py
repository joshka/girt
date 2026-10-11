import json, re, subprocess
from pathlib import Path
r=Path('/tmp/23-combined-bench');records=[]
for workload in ['changing-roots','protected']:
 for mode in ['repack','maint']:
  dest=r/f'{workload}-{mode}'
  subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(dest)],check=True)
  if workload=='protected':
   source=next((dest/'objects/pack').glob('*.pack'));source.with_suffix('.keep').write_text('benchmark protected source\n')
  for round_ in range(1,4):
   object_id=subprocess.check_output(['git','--git-dir='+str(dest),'hash-object','-w','--stdin'],input=f'changing root {round_}\n'.encode()).decode().strip()
   subprocess.run(['git','--git-dir='+str(dest),'update-ref',f'refs/tags/bench-{round_}',object_id],check=True)
   name=f'{workload}-{mode}-{round_}'
   with (r/(name+'.out')).open('w') as out,(r/(name+'.time')).open('w') as err:
    code=subprocess.run(['/usr/bin/time','-l',str(r/'target/release/bench-combined'),mode,str(dest)],stdout=out,stderr=err).returncode
   timing=(r/(name+'.time')).read_text()
   packs={p.name:p.stat().st_size for p in (dest/'objects/pack').glob('*.pack')}
   record=dict(workload=workload,mode=mode,round=round_,status=code,seconds=float(re.search(r'([\d.]+) real\s',timing)[1]),rss_bytes=int(re.search(r'(\d+)\s+maximum resident set size',timing)[1]),packs=packs,total_pack_bytes=sum(packs.values()),path=str(dest))
   records.append(record);(r/'workloads.json').write_text(json.dumps(records,indent=2));print(json.dumps(record),flush=True)
   if code: raise RuntimeError(name+' failed')
