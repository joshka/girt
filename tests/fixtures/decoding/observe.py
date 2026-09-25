"""Original fixtures observing Git commands, without upstream source input."""
import json, os, platform, subprocess, tempfile, sys
from pathlib import Path

def isolated_fsck(fmt, kind, payload):
    with tempfile.TemporaryDirectory() as root:
        env = {k:v for k,v in os.environ.items() if not k.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
        def run(*args, data=b''):
            return subprocess.run(['git',*args],cwd=root,env=env,input=data,capture_output=True)
        run('init','--bare','--template=','--object-format='+fmt,'.')
        run('mktree')
        run('hash-object','-w','--stdin',data=b'x')
        oid=run('hash-object','-w','--literally','-t',kind,'--stdin',data=payload).stdout.strip().decode()
        p=run('fsck','--strict','--no-reflogs',oid)
        return dict(code=p.returncode,out=p.stdout.hex(),err=p.stderr.hex())

def observe():
    records = []
    env = {k:v for k,v in os.environ.items() if not k.startswith('GIT_')}
    env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
    for fmt in ('sha1', 'sha256'):
      with tempfile.TemporaryDirectory() as root:
        def git(*args, data=b''):
            p = subprocess.run(['git', *args], cwd=root, env=env, input=data, capture_output=True)
            return dict(code=p.returncode, out=p.stdout.hex(), err=p.stderr.hex())
        git('init', '--bare', '--template=', '--object-format='+fmt, '.')
        tree = bytes.fromhex(git('mktree')['out']).strip()
        blob = bytes.fromhex(git('hash-object','-w','--stdin', data=b'x')['out']).strip()
        t = b'tree '+tree+b'\n'
        a = b'author A <a> 1 +0000\n'
        c = b'committer C <c> 2 +0000\n'
        cases = {'canonical': t+a+c+b'\nbody', 'tree_only':t, 'tree_no_lf':t[:-1],
          'missing_author':t+c+b'\n', 'missing_committer':t+a+b'\n',
          'reordered_people':t+c+a+b'\n', 'reordered_tree':a+t+c+b'\n',
          'duplicate_tree':t+b'tree '+b'1'*len(tree)+b'\n'+a+c+b'\n', 'duplicate_committer':t+a+c+b'committer D <d> 4 +0200\n\n', 'duplicate_author':t+a+b'author B <b> 3 +0100\n'+c+b'\n',
          'folded_tree':t+b' continuation\n'+a+c+b'\n', 'folded_author':t+a+b' continuation\n'+c+b'\n',
          'missing_space':t+a+c+b'opaque\n\n', 'tab':t+a+c+b'\tcontinued\n\n',
          'orphan':t+b' orphan\n'+a+c+b'\n', 'no_separator':t+a+c,
          'parent_only':t+b'parent '+b'1'*len(tree)+b'\n', 'short_parent':t+b'parent short\n', 'truncated_parent':t+b'parent '+b'1'*len(tree), 'truncated_person':t+a+c[:-1], 'late_parent':t+a+c+b'parent '+b'1'*len(tree)+b'\n\n'}
        for name, person in {'missing':b'A <a>', 'overflow':b'A <a> 9223372036854775808 +0000',
           'bad':b'A <a> now +0000', 'hours':b'A <a> 1 +2400', 'minutes':b'A <a> 1 -0060',
           'short':b'A <a> 1 +1', 'unsigned':b'A <a> 1 0000', 'negative':b'A <a> -1 +0000',
           'space':b' A \t<a>\t1 +0000', 'trailing':b'A <a> 1 +0000 extra',
           'suffix_zone':b'A <a> 1 +01foo', 'adjacent_zone':b'A <a> 1+0100', 'badseconds':b'A <a> 1x +0000', 'tab_date':b'A <a> 1\t+0000', 'nodate_space':b'A <a>1 +0000'}.items():
            cases['date_'+name] = t+b'author '+person+b'\n'+c+b'\n'
        def record(kind, name, payload, command):
            oid=bytes.fromhex(git('hash-object','-w','--literally','-t',kind,'--stdin',data=payload)['out']).strip().decode()
            records.append(dict(format=fmt,kind=kind,name=name,payload=payload.hex(),oid=oid,
              read=git(*command,oid), cat=git('cat-file',kind,oid),
              writer=git('hash-object','-t',kind,'--stdin',data=payload),
              fsck=isolated_fsck(fmt, kind, payload)))
            if kind == 'tag':
                records[-1]['peel'] = git('rev-parse','--verify',oid+'^{}')
                git('update-ref','refs/tags/observed',oid)
                records[-1]['metadata'] = git('for-each-ref','--format=%(taggername)|%(taggeremail)|%(taggerdate:raw)','refs/tags/observed')
            if kind == 'commit': records[-1]['graph'] = git('rev-list','--parents',oid)

        for name,payload in cases.items(): record('commit',name,payload,('show','-s','--format=%T|%P|%an|%ae|%at|%ai|%cn|%ct'))
        for mode in (b'100644',b'100664',b'100600',b'100700',b'100744',b'0100644',b'040000',b'40001',b'120777',b'160001',b'0',b'777777',b'100888',b'100000000000',b'777777777777777777777777',b'0000000000000000000000100644'):
            record('tree',mode.decode(),mode+b' x\0'+bytes.fromhex(blob.decode()),('ls-tree',))
        base=b'object '+blob+b'\ntype blob\ntag v\n'
        for name,payload in {'canonical':base+b'\nbody', 'no_separator':base,'no_lf':base[:-1],
          'missing_name':b'object '+blob+b'\ntype blob\n', 'bad_tagger':base+b'tagger A <a> now +0000\n\n',
          'duplicate_type':base+b'type tree\n\n','reordered':b'type blob\nobject '+blob+b'\ntag v\n\n',
          'folded_object':b'object '+blob+b'\n continuation\ntype blob\ntag v\n\n',
          'folded_type':b'object '+blob+b'\ntype blob\n continuation\ntag v\n\n',
          'missing_type':b'object '+blob+b'\ntag v\n\n', 'missing_object':b'type blob\ntag v\n\n',
          'folded':base+b' continuation\n\n', 'opaque':base+b'opaque\n\n', 'truncated_extra':base+b'opaque', 'misplaced_tagger':base+b'x extra\ntagger A <a> 1 +0000\n\n', 'duplicate_tagger':base+b'tagger A <a> 1 +0000\ntagger B <b> 2 +0100\n\n'}.items():
            record('tag',name,payload,('rev-parse','--verify'))
    return dict(platform=platform.platform(),git=subprocess.check_output(['git','--version']).decode().strip(),cases=records, signatures={fmt: signature_probe(fmt) for fmt in ("sha1", "sha256")})
def signature_probe(fmt):
    with tempfile.TemporaryDirectory() as root:
        env = {k:v for k,v in os.environ.items() if not k.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull)
        def git(*args, data=b''):
            p=subprocess.run(['git',*args],cwd=root,env=env,input=data,capture_output=True)
            return dict(code=p.returncode,out=p.stdout.hex(),err=p.stderr.hex())
        git('init','--bare','--template=','--object-format='+fmt,'.')
        tree=bytes.fromhex(git('mktree')['out']).strip()
        capture=Path(root)/'capture'
        capture.mkdir()
        probe=Path(root)/'probe'
        probe.write_text('#!'+sys.executable+'\nimport pathlib, sys\nroot=pathlib.Path('+repr(str(capture))+')\n'+
            '(root/"payload").write_bytes(sys.stdin.buffer.read())\n'+
            '(root/"signature").write_bytes(pathlib.Path(sys.argv[-2]).read_bytes())\nsys.exit(1)\n')
        probe.chmod(0o700)
        results={}
        for name, headers in {
            'both':b'gpgsig -----BEGIN PGP SIGNATURE-----\n first\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n second\n',
            'repeated':b'gpgsig -----BEGIN PGP SIGNATURE-----\n first\ngpgsig -----BEGIN PGP SIGNATURE-----\n second\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n third\n',
            'legacy':b'opaque\n\tlegacy\ngpgsig -----BEGIN PGP SIGNATURE-----\n first\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n second\n',
        }.items():
            for old in capture.iterdir(): old.unlink()
            payload=b'tree '+tree+b'\nauthor A <a> now +0000\ncommitter C <c> 1 +0000\n'+headers+b'\nbody\xff'
            oid=bytes.fromhex(git('hash-object','-w','--literally','-t','commit','--stdin',data=payload)['out']).strip().decode()
            result=git('-c','gpg.program='+str(probe),'verify-commit',oid)
            results[name]=dict(payload=payload.hex(),result=result,captured={p.name:p.read_bytes().hex() for p in capture.iterdir()})
        return results

if __name__=='__main__': print(json.dumps(observe(),indent=2))
