#!/usr/bin/env python3
"""Small Linux PTY boundary test: startup, prefixes, prompts, cancellation, exit."""
import argparse,fcntl,json,os,pathlib,pty,select,signal,struct,subprocess,tempfile,termios,time
from reference import ROOT

def smoke(binary):
    results=[]
    for profile,keys in [('visidata',[b'g',b'\x1b',b'g',b't',b'q']),('dataman',[b'e','α🙂'.encode(),b'\x1b',b'\x03'])]:
        with tempfile.TemporaryDirectory() as temp:
            path=pathlib.Path(temp)/'rows.csv';path.write_text('name,value\none,x\ntwo,y\n')
            master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',24,80,0,0))
            env=dict(os.environ,TERM='xterm-256color')
            p=subprocess.Popen([str(binary),'--keymap',profile,str(path)],stdin=slave,stdout=slave,stderr=slave,cwd=temp,env=env,start_new_session=True);os.close(slave)
            output=bytearray()
            def drain(duration):
                end=time.monotonic()+duration
                while time.monotonic()<end:
                    ready,_,_=select.select([master],[],[],min(.05,max(0,end-time.monotonic())))
                    if ready:
                        try:output.extend(os.read(master,65536))
                        except OSError:return
            try:
                drain(.5)
                if p.poll() is not None:raise RuntimeError('TUI failed during startup: '+output.decode(errors='replace'))
                for key in keys:os.write(master,key);drain(.15)
                p.wait(timeout=5)
                if p.returncode!=0:raise RuntimeError('TUI exit failed: '+output.decode(errors='replace'))
                if profile=='dataman' and b'Value' not in output:raise RuntimeError('input prompt was not rendered')
                if b'\x1b[' not in output:raise RuntimeError('no terminal frames rendered')
                results.append({'profile':profile,'status':'passing','bytes':len(output)})
            finally:
                if p.poll() is None:os.killpg(p.pid,signal.SIGKILL);p.wait()
                os.close(master)
    return results
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',type=pathlib.Path,default=ROOT/'target/debug/dataman');a=parser.parse_args();print(json.dumps(smoke(a.binary),indent=2))
