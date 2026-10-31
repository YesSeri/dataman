#!/usr/bin/env python3
"""Acceptance harness. Normal verification never rewrites reference snapshots."""
import time
import argparse,collections,hashlib,json,pathlib,shutil,subprocess,sys,tempfile
from reference import ROOT,LOCK,check

def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def load(path):return json.loads(path.read_text())
def run(args,deadline=None,**kw):
    remaining=120 if deadline is None else min(120,deadline-time.time())
    if remaining<=0:raise RuntimeError('run deadline exhausted')
    p=subprocess.run(args,capture_output=True,text=True,timeout=remaining,**kw)
    if p.returncode:raise RuntimeError(f'{args[0]} exited {p.returncode}: {p.stderr[-4000:]}')
    return p.stdout

def compare(expected,actual,path='$'):
    """Exact, ordered, typed comparison. No implicit tolerance or sorting."""
    if type(expected)!=type(actual):return {'path':path,'expected':expected,'actual':actual}
    if isinstance(expected,dict):
        if expected.keys()!=actual.keys():return {'path':path,'expected_keys':list(expected),'actual_keys':list(actual)}
        for k in expected:
            diff=compare(expected[k],actual[k],f'{path}.{k}')
            if diff:return diff
    elif isinstance(expected,list):
        if len(expected)!=len(actual):return {'path':path,'expected_length':len(expected),'actual_length':len(actual)}
        for i,(e,a) in enumerate(zip(expected,actual)):
            diff=compare(e,a,f'{path}[{i}]')
            if diff:return diff
    elif expected!=actual:return {'path':path,'expected':expected,'actual':actual}
    return None

def project(state,normalization):
    if normalization!={'omit_dataman_synthetic_id':True,'projection':'data-selection'}:
        raise RuntimeError('unknown normalization contract')
    sheets=[]
    for s in state['sheets']:
        indexes=[i for i,c in enumerate(s['columns']) if c!='id']
        sheets.append({'alias':s['alias'],'columns':[s['columns'][i] for i in indexes],
            'rows':[{'ref':r['ref'],'cells':[r['cells'][i] for i in indexes],'selected':r['selected']} for r in s['rows']]})
    return {'sheets':sheets}

def execute(scenario,reference=False,binary=None,deadline=None,overrides=None):
    # Both applications only see private copies, including SQLite databases.
    with tempfile.TemporaryDirectory(prefix='dataman-parity-') as temp:
        temp=pathlib.Path(temp);paths=[]
        if overrides is not None:(temp/'keybindings.toml').write_text('[bindings]\n'+''.join(json.dumps(k)+' = '+json.dumps(v)+'\n' for k,v in overrides.items()))
        for fixture in scenario['fixtures']:
            dest=temp/pathlib.Path(fixture).name;shutil.copy2(ROOT/fixture,dest);paths.append(str(dest))
        if reference:
            return json.loads(run([check(),str(ROOT/'tools/vd_adapter.py')],deadline=deadline,input=json.dumps({'paths':paths,'steps':scenario['visidata']}),cwd=temp))
        results=[]
        for stop in scenario['checkpoints']:
            workflow=temp/'workflow.jsonl';state=temp/'state.json'
            records=[{'kind':'header','schema_version':1}]+scenario['native'][:stop]
            workflow.write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in records))
            run([str(binary or ROOT/'target/debug/dataman'),*paths,'--batch','--play',str(workflow),'--dump-state',str(state)],deadline=deadline,cwd=temp)
            results.append(project(load(state),scenario['normalization']))
        return results

def metadata(scenario):
    return {'schema_version':1,'reference':LOCK,'scenario_sha256':digest(ROOT/'parity/scenarios.json'),
        'dependency_lock_sha256':digest(ROOT/'parity/requirements.lock'),'adapter_sha256':digest(ROOT/'tools/vd_adapter.py'),
        'fixtures':{f:digest(ROOT/f) for f in scenario['fixtures']}}

def verify(ids=None,binary=None,update=False,deadline=None):
    results=[]
    try:check();infrastructure=None
    except Exception as error:infrastructure=str(error)
    scenarios=load(ROOT/'parity/scenarios.json')['scenarios']
    for s in scenarios:
        if ids and s['id'] not in ids:continue
        result={'scenario':s['id'],'dimension':s['dimension']}
        if infrastructure:
            result.update(status='infrastructure_failure',reason=infrastructure);results.append(result);continue
        if s.get('deferred'):
            result.update(status='deferred');results.append(result);continue
        if s.get('unverified'):
            result.update(status='unverified',reason=s['unverified']);results.append(result);continue
        if s.get('blocked'):
            result.update(status='blocked',reason=s['blocked']);results.append(result);continue
        try:
            path=ROOT/'parity/references'/f"{s['id']}.json"
            if update:
                checkpoints=execute(s,reference=True)
                expected=[checkpoints[i] for i in s['checkpoints']]
                path.write_text(json.dumps({'metadata':metadata(s),'checkpoints':expected},indent=2,ensure_ascii=False)+'\n')
                result['status']='reference_updated'
            else:
                stored=load(path)
                if stored['metadata']!=metadata(s):raise RuntimeError('reference version, scenario, or fixture hash mismatch; explicit reference-update required')
                actual=execute(s,binary=binary,deadline=deadline)
                diff=compare(stored['checkpoints'],actual)
                result.update(status='failing' if diff else 'passing',difference=diff)
                if diff:
                    import re
                    coordinates=re.findall(r'\[(\d+)\]',diff['path']);result['location']={'checkpoint':int(coordinates[0]) if coordinates else None,'sheet':int(coordinates[1]) if len(coordinates)>1 else None,'row':int(coordinates[2]) if len(coordinates)>2 else None,'column':int(coordinates[3]) if len(coordinates)>3 else None}
        except Exception as error:
            # Dataman command errors are acceptance failures; reference/setup failures are infrastructure.
            result.update(status='infrastructure_failure' if update or not path.exists() or 'mismatch' in str(error) else 'failing',reason=str(error))
        results.append(result)
    return {'schema_version':1,'results':results,'coverage':{dimension:dict.fromkeys(['passing','failing','unverified','blocked','deferred','infrastructure_failure'],0) | dict(collections.Counter(r['status'] for r in results if r['dimension']==dimension)) for dimension in ['behavioral','interaction']}}

def report(result):
    for r in result['results']:print(f"{r['status']:24} {r['scenario']}: {r.get('difference') or r.get('reason','')}")
    print(json.dumps(result['coverage'],indent=2))

def main():
    parser=argparse.ArgumentParser();parser.add_argument('command',choices=['verify','reference-update','report']);parser.add_argument('--scenario',action='append');parser.add_argument('--binary',type=pathlib.Path);parser.add_argument('--output',type=pathlib.Path,default=ROOT/'.parity/results.json');args=parser.parse_args()
    if args.command=='report':report(load(args.output));return
    result=verify(args.scenario,args.binary,args.command=='reference-update');args.output.parent.mkdir(parents=True,exist_ok=True);args.output.write_text(json.dumps(result,indent=2)+'\n');report(result)
    if any(r['status'] in ['failing','infrastructure_failure'] for r in result['results']):sys.exit(1)
if __name__=='__main__':main()
