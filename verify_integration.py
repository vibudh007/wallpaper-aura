# SPDX-License-Identifier: GPL-3.0-only
import os,json,selectors,subprocess,tempfile,time
from pathlib import Path
from PIL import Image
B=Path(__file__).resolve().parent/'wallpaper-aura'
class Daemon:
 def __init__(self,args,env=None):
  self.p=subprocess.Popen([str(B),*args,'--dry-run'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=env)
  self.sel=selectors.DefaultSelector();self.sel.register(self.p.stdout,selectors.EVENT_READ)
 def read(self):
  assert self.sel.select(4),'missing palette event'
  line=self.p.stdout.readline();assert line, self.p.stderr.read()
  return json.loads(line)
 def close(self):
  self.p.terminate();self.p.wait(timeout=2);self.sel.close();self.p.stdout.close();self.p.stderr.close()
 def resources(self):
  root=Path(f'/proc/{self.p.pid}')
  rss=int(next(s.split()[1] for s in (root/'status').read_text().splitlines() if s.startswith('VmRSS')))
  return len(list((root/'fd').iterdir())),len(list((root/'task').iterdir())),rss
with tempfile.TemporaryDirectory() as tmp:
 d=Path(tmp).resolve();img=d/'wall.png'
 def image(path,rgb):Image.new('RGB',(256,128),rgb).save(path,compress_level=0)
 image(img,(128,20,40)); daemon=Daemon(['--wallpaper',str(img)])
 try:
  first=daemon.read()['colors'];baseline=daemon.resources()
  # Same size and mtime must still invalidate cache on atomic replacement.
  stamp=img.stat().st_mtime_ns;new=d/'new.png';image(new,(20,128,40));assert new.stat().st_size==img.stat().st_size
  os.utime(new,ns=(stamp,stamp));new.replace(img);assert daemon.read()['colors']!=first
  for n in range(40):
   image(new,(130+n%2*20,20,40));new.replace(img);daemon.read()
  after=daemon.resources();assert after[:2]==baseline[:2],(baseline,after)
  assert after[2]-baseline[2]<8192,(baseline,after)
  print('replacement + 40 updates: stable descriptors/threads; RSS KiB',baseline[2],after[2])
 finally:daemon.close()
 # Replacing the watched directory must re-arm its watch for later edits.
 folder=d/'folder';folder.mkdir();target=folder/'bg.png';image(target,(128,20,40));daemon=Daemon(['--wallpaper',str(target)])
 try:
  daemon.read();folder.rename(d/'old-folder');folder.mkdir();image(target,(20,128,40));daemon.read()
  image(folder/'next.png',(40,20,128));(folder/'next.png').replace(target);daemon.read()
  print('watched directory replacement: passed')
 finally:daemon.close()
 # Missing startup image must recover without restarting or polling.
 missing=d/'later.png';daemon=Daemon(['--wallpaper',str(missing)])
 try:time.sleep(.1);image(missing,(128,40,80));assert daemon.read()['colors'];print('missing startup image recovery: passed')
 finally:daemon.close()
 # Malformed COSMIC state at startup must remain watched and recover.
 state=d/'state/cosmic/com.system76.CosmicBackground/v1/wallpapers';state.parent.mkdir(parents=True);state.write_text('invalid state')
 env=os.environ.copy();env['XDG_STATE_HOME']=str(d/'state');daemon=Daemon(['--backend','cosmic'],env)
 try:
  time.sleep(.1);state.write_text(f'[("eDP-1",Path({json.dumps(str(img))}))]');assert daemon.read()['colors'];print('COSMIC startup recovery: passed')
 finally:daemon.close()
 # Helper EOF during idle must fail promptly, allowing systemd recovery.
 helpers=d/'helpers';helpers.mkdir();helper=helpers/'gsettings'
 helper.write_text('#!/usr/bin/python3\nimport sys,time\nif sys.argv[1]=="monitor":\n time.sleep(.5)\n sys.exit(1)\nprint('+repr("'"+img.as_uri()+"'")+')\n')
 helper.chmod(0o700);env=os.environ.copy();env['PATH']=str(helpers)+os.pathsep+env['PATH'];daemon=Daemon(['--backend','gnome'],env)
 try:
  daemon.read();assert daemon.p.wait(timeout=3)!=0;print('notification helper EOF recovery: passed')
 finally:daemon.close()
 # A FIFO must be rejected immediately instead of hanging.
 fifo=d/'fifo';os.mkfifo(fifo)
 for flag in ['--wallpaper','--path-file']:
  p=subprocess.run([str(B),flag,str(fifo),'--dry-run','--once'],capture_output=True,timeout=2)
  assert p.returncode!=0
 print('FIFO inputs rejected: passed')
 print('integration checks passed')
