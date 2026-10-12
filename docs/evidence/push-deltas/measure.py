# Run from the repository root on macOS, under the shared timing lock.
# See ../push-deltas.md for fixture and release-driver setup.
import subprocess,pathlib,json
r=pathlib.Path('/tmp/girt-25-evidence'); src=r/'edit-source.git'; dst=r/'edit-dest.git'
def git(p,*args,input=None): return subprocess.run(['git','-C',str(p),*args],input=input,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout
subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(src)],check=True)
state=0x12345678; data=bytearray()
for _ in range(512*1024):
 state ^= (state<<13)&0xffffffff; state ^= state>>17; state ^= (state<<5)&0xffffffff; data.append(state&255)
parent=git(src,'rev-parse','HEAD').decode().strip()
def commit(data,parent):
 blob=git(src,'hash-object','-w','--stdin',input=data).decode().strip()
 entries=git(src,'ls-tree',parent+'^{tree}')+f'100644 blob {blob}\tgirt-delta-fixture.bin\n'.encode()
 # Replace fixture entry when present.
 lines=entries.splitlines(); entries=b'\n'.join(x for x in lines[:-1] if not x.endswith(b'\tgirt-delta-fixture.bin'))+b'\n'+lines[-1]+b'\n'
 tree=git(src,'mktree',input=entries).decode().strip()
 return git(src,'-c','user.name=Delta','-c','user.email=delta@example.com','commit-tree',tree,'-p',parent,input=b'delta fixture\n').decode().strip()
old=commit(data,parent); git(src,'update-ref','refs/heads/bench-push',old)
subprocess.run(['cp','-cR',str(src),str(dst)],check=True)
data[262144:262152]=b'EDITED!!'; new=commit(data,old); git(src,'update-ref','refs/heads/bench-push',new)
(r/'edit-ids.json').write_text(json.dumps(dict(old=old,new=new)))
print(old,new)

import subprocess,pathlib,json,re,time,os
r=pathlib.Path('/tmp/girt-25-evidence'); ids=json.loads((r/'edit-ids.json').read_text()); records=[]
def run(args,**kw): return subprocess.run(args,check=True,**kw)
portfile=r/'port'; server=subprocess.Popen(['python3','/Users/joshka/local/girt/work/25-push-deltas/scripts/jj-e2e/http_server.py',str(r),str(portfile)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
try:
 for _ in range(100):
  if portfile.exists(): break
  if server.poll() is not None: raise RuntimeError('HTTP server exited')
  time.sleep(.05)
 port=portfile.read_text().strip()
 for n in range(3):
  for transport in ['local','http']:
   for op in ['new','ff','edit']:
    for impl in ['main','after','git']:
     label=f'accepted-{transport}-{op}-{n}-{impl}'; p=r/label; src=r/(label+'.git'); dst=r/(label+'.dest.git')
     run(['cp','-cR','/tmp/opus-med-23/source.git',str(src)])
     run(['cp','-cR','/tmp/opus-med-23/source.git',str(dst)])
     old=ids['old'] if op=='edit' else '987c084b8f3ff28e8246d8b38c55c323c85611d5'; new=ids['new'] if op=='edit' else '812a3cdbfb334543bbd5858335e21fc5c941287b'
     overlay=r/'edit-source.git' if op=='edit' else pathlib.Path('/tmp/perf-audit/pushsrc.git')
     run(['cp','-cR',str(overlay/'objects')+'/.',str(src/'objects')])
     if op=='edit': run(['cp','-cR',str(r/'edit-dest.git/objects')+'/.',str(dst/'objects')])
     expected=old if op!='new' else '-'
     if op!='new': run(['git','-C',str(dst),'update-ref','refs/heads/bench-push',old])
     run(['git','-C',str(src),'update-ref','refs/heads/bench-push',new])
     run(['git','-C',str(dst),'config','http.receivepack','true'])
     url=str(dst) if transport=='local' else f'http://127.0.0.1:{port}/{dst.name}'
     run(['git','-C',str(src),'config','remote.origin.url',url])
     driver=str(r/'main-driver') if impl=='main' else '/tmp/girt-25-driver/target/release/perfdrv'
     cmd=['git','-C',str(src),'push','-q','origin','bench-push'] if impl=='git' else [driver,'push',str(src),new,expected]
     with (r/(label+'.log')).open('w') as log: run(['/usr/bin/time','-l',*cmd],stdout=log,stderr=log)
     text=(r/(label+'.log')).read_text(); seconds=float(re.search(r'([\d.]+) real',text).group(1)); rss=int(re.search(r'(\d+)  maximum resident set size',text).group(1))
     records.append(dict(transport=transport,op=op,n=n,impl=impl,seconds=seconds,rss=rss)); print(records[-1],flush=True)
     run(['git','-C',str(dst),'fsck','--connectivity-only'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
     if op=='edit':
      actual=run(['git','-C',str(dst),'show',new+':girt-delta-fixture.bin'],stdout=subprocess.PIPE).stdout
      source=run(['git','-C',str(src),'show',new+':girt-delta-fixture.bin'],stdout=subprocess.PIPE).stdout
      assert actual==source
     (r/'measurements.json').write_text(json.dumps(records,indent=2))
finally:
 server.terminate(); server.wait()
 portfile.unlink(missing_ok=True)

import subprocess,pathlib,json
r=pathlib.Path('/tmp/girt-25-evidence'); ids=json.loads((r/'edit-ids.json').read_text()); src=r/'phase-final.git'
subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(src)],check=True)
subprocess.run(['cp','-cR',str(r/'edit-source.git/objects')+'/.',str(src/'objects')],check=True)
for impl,driver in [('main',r/'main-driver'),('after',pathlib.Path('/tmp/girt-25-driver/target/release/perfdrv'))]:
 out=subprocess.run(['/usr/bin/time','-l',str(driver),'prepare-push',str(src),ids['new'],ids['old']],capture_output=True,text=True,check=True)
 (r/f'final-phase-{impl}.log').write_text(out.stdout+out.stderr); print(impl,out.stdout,out.stderr)
pack=subprocess.run(['git','-C',str(src),'pack-objects','--stdout','--revs','--thin'],input=(ids['new']+'\n^'+ids['old']+'\n').encode(),capture_output=True,check=True).stdout
(r/'git-pack-size.txt').write_text(str(len(pack))+'\n'); print('git pack bytes',len(pack))
