import subprocess,pathlib,json
r=pathlib.Path('/tmp/girt-25-evidence'); ids=json.loads((r/'edit-ids.json').read_text()); src=r/'phase-final.git'
subprocess.run(['cp','-cR','/tmp/opus-med-23/source.git',str(src)],check=True)
subprocess.run(['cp','-cR',str(r/'edit-source.git/objects')+'/.',str(src/'objects')],check=True)
for impl,driver in [('main',r/'main-driver'),('after',pathlib.Path('/tmp/girt-25-driver/target/release/perfdrv'))]:
 out=subprocess.run(['/usr/bin/time','-l',str(driver),'prepare-push',str(src),ids['new'],ids['old']],capture_output=True,text=True,check=True)
 (r/f'final-phase-{impl}.log').write_text(out.stdout+out.stderr); print(impl,out.stdout,out.stderr)
pack=subprocess.run(['git','-C',str(src),'pack-objects','--stdout','--revs','--thin'],input=(ids['new']+'\n^'+ids['old']+'\n').encode(),capture_output=True,check=True).stdout
(r/'git-pack-size.txt').write_text(str(len(pack))+'\n'); print('git pack bytes',len(pack))
