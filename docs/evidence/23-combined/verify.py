import json,subprocess
from pathlib import Path
root=Path('/tmp/23-combined-bench')
paths=[root/'repack-1-0-combined',root/'maint-1-0-combined']
paths += [root/(workload+'-'+mode) for workload in ['changing-roots','protected','recent'] for mode in ['repack','maint']]
results=[]
for path in paths:
 with (root/(path.name+'-fsck.txt')).open('w') as log:
  subprocess.run(['git','--git-dir='+str(path),'fsck','--strict'],stdout=log,stderr=subprocess.STDOUT,check=True)
 objects=0;deltas=0;depth=0;unique=set()
 for index in sorted((path/'objects/pack').glob('*.idx')):
  output=subprocess.check_output(['git','verify-pack','-v',str(index)],text=True)
  for line in output.splitlines():
   fields=line.split()
   if len(fields) not in (5,7) or len(fields[0])!=40:continue
   unique.add(fields[0]);objects+=1
   if len(fields)==7:deltas+=1;depth=max(depth,int(fields[5]))
 record={'path':str(path),'fsck':'pass','verify_pack':'pass','pack_count':len(list((path/'objects/pack').glob('*.pack'))),'packed_entries':objects,'unique_packed_objects':len(unique),'deltas':deltas,'max_depth':depth}
 results.append(record);print(json.dumps(record),flush=True)
 (root/'integrity.json').write_text(json.dumps(results,indent=2)+'\n')
