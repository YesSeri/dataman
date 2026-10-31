#!/usr/bin/env python3
"""Explicit project-local reference provisioning. Never discovers a system vd."""
import hashlib, json, os, pathlib, platform, subprocess, sys, tarfile, urllib.request, venv
ROOT=pathlib.Path(__file__).resolve().parents[1]
LOCK=json.loads((ROOT/'parity/reference-lock.json').read_text())
ENV=ROOT/'.parity/venv'
def setup():
    if platform.python_version()!=LOCK['python']:
        raise RuntimeError(f"reference requires Python {LOCK['python']}, got {platform.python_version()}")
    local=ROOT/'.parity';local.mkdir(exist_ok=True)
    archive=local/'visidata.tar.gz'
    urllib.request.urlretrieve(LOCK['source_url'],archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest()!=LOCK['source_sha256']:
        raise RuntimeError('reference source checksum mismatch')
    source=local/'source';source.mkdir(exist_ok=True)
    with tarfile.open(archive) as tf:
        for member in tf.getmembers():
            dest=(source/member.name).resolve()
            if not dest.is_relative_to(source.resolve()):raise RuntimeError('unsafe source archive')
            if member.issym() or member.islnk():
                target=(dest.parent/member.linkname).resolve() if member.issym() else (source/member.linkname).resolve()
                if not target.is_relative_to(source.resolve()):raise RuntimeError('unsafe source archive link')
        tf.extractall(source)
    venv.EnvBuilder(with_pip=True,symlinks=True).create(ENV)
    python=str(ENV/'bin/python')
    subprocess.run([python,'-m','pip','install','--require-hashes','-r',str(ROOT/'parity/requirements.lock')],check=True)
    subprocess.run([python,'-m','pip','install','--no-deps','--no-build-isolation',str(source/'visidata-3.5')],check=True)
    (local/'provision.json').write_text(json.dumps(LOCK,indent=2)+'\n')
    check()
def check():
    if not (ENV/'bin/python').exists():raise RuntimeError('reference unavailable: run just parity-setup')
    code='import visidata,platform,json; from importlib.metadata import version; print(json.dumps([visidata.__version__,platform.python_version(),version("python-dateutil"),version("six")]))'
    actual=json.loads(subprocess.check_output([str(ENV/'bin/python'),'-c',code]))
    if actual!=[LOCK['version'],LOCK['python'],'2.9.0.post0','1.17.0']:raise RuntimeError(f'reference version mismatch: {actual}')
    if json.loads((ROOT/'.parity/provision.json').read_text())!=LOCK:raise RuntimeError('reference provenance mismatch')
    return str(ENV/'bin/python')
if __name__=='__main__':setup()
