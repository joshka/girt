from pathlib import Path
import subprocess
resources=Path(__file__).resolve().parent
root=Path('/tmp/23-combined-bench'); root.mkdir(exist_ok=True)
for name,rev in [('opus-low','13d7e7cc7f840e0bfb69c4e02574a1a015eab339'),('sol-medium','f6221563e22088f6b1acf20c0e57c3067145ed9c')]:
 dest=root/name; dest.mkdir(exist_ok=True)
 files=subprocess.check_output(['jj','file','list','-r',rev],text=True).splitlines()
 for f in files:
  if not (f.startswith('src/') or f in ['Cargo.toml','Cargo.lock','build.rs','rust-toolchain.toml']): continue
  p=dest/f;p.parent.mkdir(parents=True,exist_ok=True)
  p.write_bytes(subprocess.check_output(['jj','file','show','-r',rev,f]))
 driver=root/(name+'-driver');(driver/'src').mkdir(parents=True,exist_ok=True)
 (driver/'src/main.rs').write_bytes((resources/'driver.rs').read_bytes())
 (driver/'Cargo.toml').write_text(f'[package]\nname="bench-{name}"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ngirt={{path="{dest}"}}\n')
 (driver/'Cargo.lock').write_bytes((dest/'Cargo.lock').read_bytes())
driver=root/'combined-driver';(driver/'src').mkdir(parents=True,exist_ok=True)
(driver/'src/main.rs').write_bytes((resources/'driver.rs').read_bytes())
(driver/'Cargo.toml').write_text('[package]\nname="bench-combined"\nversion="0.1.0"\nedition="2024"\n[dependencies]\ngirt={path="/Users/joshka/local/girt/work/23-combined"}\n')
(driver/'Cargo.lock').write_bytes(Path('Cargo.lock').read_bytes())

(driver/'src/bin').mkdir(exist_ok=True)
(driver/'src/bin/stages.rs').write_bytes((resources/'stages.rs').read_bytes())
for name in ['measure.py','run.sh','workloads.py','workloads.sh']:
 (root/name).write_bytes((resources/name).read_bytes())
