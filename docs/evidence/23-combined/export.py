from pathlib import Path
import subprocess
root=Path('/tmp/23-combined-bench'); root.mkdir(exist_ok=True)
for name,rev in [('opus-low','issue-23-opus-low@origin'),('sol-medium','issue-23-sol-med@origin')]:
 dest=root/name; dest.mkdir(exist_ok=True)
 files=subprocess.check_output(['jj','file','list','-r',rev],text=True).splitlines()
 for f in files:
  if not (f.startswith('src/') or f in ['Cargo.toml','Cargo.lock','build.rs','rust-toolchain.toml']): continue
  p=dest/f;p.parent.mkdir(parents=True,exist_ok=True)
  p.write_bytes(subprocess.check_output(['jj','file','show','-r',rev,f]))
 driver=root/(name+'-driver');(driver/'src').mkdir(parents=True,exist_ok=True)
 (driver/'src/main.rs').write_bytes(Path('/tmp/girt-quiet-bench/driver.rs').read_bytes())
 (driver/'Cargo.toml').write_text(f'[package]\nname="bench-{name}"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ngirt={{path="{dest}"}}\n')
 (driver/'Cargo.lock').write_bytes((dest/'Cargo.lock').read_bytes())
driver=root/'combined-driver';(driver/'src').mkdir(parents=True,exist_ok=True)
(driver/'src/main.rs').write_bytes(Path('/tmp/girt-quiet-bench/driver.rs').read_bytes())
(driver/'Cargo.toml').write_text('[package]\nname="bench-combined"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ngirt={path="/Users/joshka/local/girt/work/23-combined"}\n')
(driver/'Cargo.lock').write_bytes(Path('Cargo.lock').read_bytes())
