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
     label=f'final-{transport}-{op}-{n}-{impl}'; p=r/label; src=r/(label+'.git'); dst=r/(label+'.dest.git')
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
