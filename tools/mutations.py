#!/usr/bin/env python3
"""Explicit negative controls: confirm meaningful changes cannot silently pass."""
import argparse,copy,json,pathlib,tempfile,subprocess
from parity import ROOT,load,execute,compare

def prove(binary):
    scenarios={s['id']:s for s in load(ROOT/'parity/scenarios.json')['scenarios']}
    name='invert-empty-selection-interaction';scenario=scenarios[name]
    expected=load(ROOT/f'parity/references/{name}.json')['checkpoints']
    actual=execute(scenario,binary=binary)
    if compare(expected,actual):raise RuntimeError('positive control must pass before mutation checks')
    results=[]
    for label,mutate in [('value',lambda x:x[-1]['sheets'][0]['rows'][0]['cells'][0].__setitem__('Text','wrong')),
                         ('selection',lambda x:x[-1]['sheets'][0]['rows'][0].__setitem__('selected',False))]:
        changed=copy.deepcopy(actual);mutate(changed)
        diff=compare(expected,changed)
        if not diff:raise RuntimeError(f'{label} mutation escaped')
        results.append({'mutation':label,'status':'detected','difference':diff})
    diff=compare(expected,execute(scenario,binary=binary,overrides={'gt':'clear_selection'}))
    if not diff:raise RuntimeError('keybinding mutation escaped')
    results.append({'mutation':'keybinding','status':'detected','difference':diff})
    # Compare observable prompt state, without asserting unverified VisiData prompt equivalence.
    with tempfile.TemporaryDirectory() as tmp:
        tmp=pathlib.Path(tmp);source=tmp/'rows.csv';source.write_text('name\na\n')
        workflow=tmp/'workflow.jsonl';workflow.write_text('{"kind":"header","schema_version":1}\n{"kind":"key","key":"e"}\n')
        state=tmp/'state.json';subprocess.run([str(binary),str(source),'--keymap','dataman','--batch','--play',str(workflow),'--dump-state',str(state)],check=True,cwd=tmp)
        original=load(state);changed=copy.deepcopy(original);changed['prompt']['label']='Wrong prompt'
        diff=compare(original,changed)
        if not diff:raise RuntimeError('prompt mutation escaped')
        results.append({'mutation':'prompt','status':'detected','difference':diff})
    return results
if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('--binary',type=pathlib.Path,default=ROOT/'target/debug/dataman');args=p.parse_args();print(json.dumps(prove(args.binary),indent=2))
