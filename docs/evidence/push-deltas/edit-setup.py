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
