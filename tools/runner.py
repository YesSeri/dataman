#!/usr/bin/env python3
"""Durable local task orchestration. No merge/push; paid invocations are opt-in."""
import argparse,contextlib,fcntl,hashlib,json,os,pathlib,signal,subprocess,sys,time,uuid
from parity import ROOT,load,verify

PROTECTED=('parity/','tools/','tests/','justfile','Cargo.toml','Cargo.lock','.gitignore')
MAX_TASKS=3
MAX_ATTEMPTS=2
DEADLINE=7200
SCHEMA={'type':'object','properties':{'status':{'type':'string','enum':['complete','blocked','approved','rejected']},'summary':{'type':'string'},'findings':{'type':'array','items':{'type':'string'}}},'required':['status','summary','findings'],'additionalProperties':False}
class StopRun(RuntimeError):pass

def atomic(path,value):
    tmp=path.with_suffix('.tmp');tmp.write_text(json.dumps(value,indent=2)+'\n');os.replace(tmp,path)
def git(cwd,*args):return subprocess.check_output(['git','-C',str(cwd),*args],text=True).strip()
def protected(name):return any(name==p or name.startswith(p) for p in PROTECTED)

def process(argv,cwd,log,deadline,stdin=None):
    remaining=deadline-time.time()
    if remaining<=0:raise StopRun('two-hour deadline exhausted')
    with open(log,'w') as output:
        p=subprocess.Popen(argv,cwd=cwd,stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,stdout=output,stderr=subprocess.STDOUT,text=True,start_new_session=True)
        try:
            p.communicate(stdin,timeout=remaining)
        except (subprocess.TimeoutExpired,KeyboardInterrupt):
            os.killpg(p.pid,signal.SIGTERM)
            try:p.wait(timeout=3)
            except subprocess.TimeoutExpired:os.killpg(p.pid,signal.SIGKILL);p.wait()
            raise StopRun('interrupted or deadline exhausted; partial work preserved')
    text=pathlib.Path(log).read_text()
    if any(s in text.lower() for s in ['authentication failed','not logged in','unauthorized','invalid api key','401 unauthorized']):
        raise StopRun('authentication failure')
    if p.returncode:raise RuntimeError(f'process exited {p.returncode}; see {log}')
    return text

def preflight(agent):
    helptext=subprocess.check_output([agent,'exec','--help'],text=True,stderr=subprocess.STDOUT)
    for flag in ['--json','--output-schema','--output-last-message','--sandbox']:
        if flag not in helptext:raise RuntimeError(f'Codex CLI missing required flag {flag}')

def invoke(agent,worktree,directory,prompt,review,deadline):
    directory.mkdir(parents=True,exist_ok=True)
    (directory/'prompt.txt').write_text(prompt)
    atomic(directory/'schema.json',SCHEMA)
    argv=[agent,'exec','--json','--output-schema',str(directory/'schema.json'),'--output-last-message',str(directory/'result.json')]
    # Implementation inherits configured model and workspace sandbox. Reviewer is explicitly read-only.
    argv+=['--sandbox','read-only' if review else 'workspace-write']
    argv+=['-C',str(worktree),'-']
    process(argv,worktree,directory/'events.jsonl',deadline,prompt)
    result=load(directory/'result.json')
    if set(result)!=set(SCHEMA['required']) or result['status'] not in SCHEMA['properties']['status']['enum'] or not isinstance(result['summary'],str) or not isinstance(result['findings'],list) or any(not isinstance(f,str) for f in result['findings']):
        raise RuntimeError('agent final result violates schema')
    return result

def changed(cwd,base):
    tracked=git(cwd,'diff','--name-only',base).splitlines()
    return sorted(set(tracked+git(cwd,'ls-files','--others','--exclude-standard').splitlines()))

def verify_task(task,worktree,directory,deadline):
    process(['cargo','build','--locked'],worktree,directory/'build.log',deadline)
    result=verify(task['acceptance_scenarios'],worktree/'target/debug/dataman',deadline=deadline)
    atomic(directory/'acceptance.json',result)
    if not result['results'] or any(r['status']!='passing' for r in result['results']):return False
    process(['cargo','test','--locked','--lib','--bins'],worktree,directory/'regression.log',deadline)
    process(['cargo','fmt','--check'],worktree,directory/'format.log',deadline)
    process([sys.executable,str(ROOT/'tools/pty_smoke.py'),'--binary',str(worktree/'target/debug/dataman')],worktree,directory/'pty.log',deadline)
    for check in task.get('verification_commands',[]):
        argv=[str(worktree/'target/debug/dataman') if item=='{binary}' else str(ROOT/item[7:]) if item.startswith('{root}/') else item for item in check]
        process(argv,worktree,directory/'negative-controls.log',deadline)
    return True

def select_ready(backlog,state):
    completed={k for k,v in state['tasks'].items() if v['state']=='verified_awaiting_review'}
    for task in backlog['tasks']:
        if not task['execution_ready'] or task['design_decisions']:continue
        entry=state['tasks'].get(task['id'])
        if entry and entry['state'] in ['verified_awaiting_review','blocked']:continue
        if set(task['dependencies'])<=completed:return task
    return None

def check_trusted(base):
    for name in git(ROOT,'ls-tree','-r','--name-only',base).splitlines():
        if protected(name):
            expected=subprocess.check_output(['git','-C',str(ROOT),'show',base+':'+name])
            if not (ROOT/name).is_file() or (ROOT/name).read_bytes()!=expected:
                raise StopRun('trusted contract differs from committed base: '+name)

def run_loop(directory,agent):
    state=load(directory/'state.json');backlog=load(ROOT/'parity/backlog.json')
    save=lambda:atomic(directory/'state.json',state)
    # Contract identity is durable, preventing altered contracts on resume.
    contract_hash=hashlib.sha256((ROOT/'parity/backlog.json').read_bytes()).hexdigest()
    if contract_hash!=state['backlog_hash']:raise RuntimeError('backlog changed since run started')
    preflight(agent)
    check_trusted(state["base"])
    try:
        while time.time()<state['deadline']:
            task=select_ready(backlog,state)
            if task is None:break
            ident=task['id']
            if ident not in state['tasks']:
                if not ident.startswith('foundation-') and state['processed']>=MAX_TASKS:raise StopRun('three-task limit exhausted')
                if not ident.startswith('foundation-'):state['processed']+=1
                state['tasks'][ident]={'state':'pending','attempts':[]}
            entry=state['tasks'][ident]
            # Foundation tasks are pre-existing in the committed base. Verify externally, never assert passing.
            if ident.startswith('foundation-'):
                entry['state']='verifying';save()
                baseline=directory/'baseline'
                if not baseline.exists():git(ROOT,'worktree','add','--detach',str(baseline),state['base'])
                if not state.get('foundation_checks'):
                    process(['cargo','build','--locked'],baseline,directory/'foundation-build.log',state['deadline'])
                    process(['cargo','test','--locked','--lib','--bins'],baseline,directory/f'{ident}-regression.log',state['deadline'])
                    process([sys.executable,'-m','unittest','discover','-s',str(baseline/'tools'),'-p','test_*.py'],baseline,directory/f'{ident}-tooling.log',state['deadline'])
                    process([sys.executable,str(baseline/'tools/pty_smoke.py'),'--binary',str(baseline/'target/debug/dataman')],baseline,directory/'foundation-pty.log',state['deadline'])
                    state['foundation_checks']=True;save()
                entry.update(state='verified_awaiting_review',commit=state['tip'],evidence='foundation regression, tooling, and PTY checks');save();continue
            worktree=directory/ident
            if not worktree.exists():
                git(ROOT,'worktree','add','--detach',str(worktree),state['tip'])
                entry['base']=state['tip'];save()
            # Recover commit made immediately before interruption; do not duplicate it.
            head=git(worktree,'rev-parse','HEAD')
            if head!=entry['base']:
                if git(worktree,'log','-1','--format=%s')==f'parity: {ident}' and entry['state']=='committing':
                    if git(ROOT,'rev-parse',state['branch'])!=head:git(ROOT,'update-ref',f"refs/heads/{state['branch']}",head,state['tip'])
                    state['tip']=head;entry.update(state='verified_awaiting_review',commit=head);save();continue
                raise StopRun('unexpected task commit; inspect preserved worktree')
            if not entry['attempts']:
                evidence=directory/f'{ident}-expected-failure';evidence.mkdir(exist_ok=True)
                if verify_task(task,worktree,evidence,state['deadline']):
                    entry.update(state='blocked',reason='acceptance already passing; task contract needs maintenance');save();continue
                observed=load(evidence/'acceptance.json')['results']
                if any(r['status']=='infrastructure_failure' for r in observed):raise StopRun('reference infrastructure failure')
                entry['expected_failure']=str(evidence/'acceptance.json');save()
            while len(entry['attempts'])<MAX_ATTEMPTS:
                number=len(entry['attempts'])+1
                attempt={'number':number,'state':'implementing'};entry['attempts'].append(attempt)
                attemptdir=directory/f'{ident}-attempt-{number}';attemptdir.mkdir(exist_ok=True)
                entry['state']='implementing';save()
                prompt=f"Implement this parity task in the current isolated worktree. Do not commit, merge, push, change contracts, tests under tests/, tools/, parity/, manifests, or runner policy. Use the configured sandbox. If approval is required return blocked. Task: {json.dumps(task)}. Expected failing evidence: {entry['expected_failure']}. Reference snapshots are in parity/references. Only implementation files may change. Run cargo fmt. Previous attempt findings: {json.dumps(entry['attempts'][:-1])}. Return the required JSON result."
                try:
                    result=invoke(agent,worktree,attemptdir/'implementation',prompt,False,state['deadline']);check_trusted(state['base']);
                    if git(worktree,'rev-parse','HEAD')!=entry['base']:raise StopRun('implementation created an unexpected commit')
                    attempt['implementation']=result;save()
                    if result['status']=='blocked':entry.update(state='blocked',reason=result['summary']);save();break
                    files=changed(worktree,entry['base'])
                    if any(protected(f) for f in files):raise RuntimeError(f'protected-file modification: {files}')
                    if not files:raise RuntimeError('implementation produced no changes')
                    entry['state']='verifying';attempt['state']='verifying';save()
                    if not verify_task(task,worktree,attemptdir,state['deadline']):raise RuntimeError('acceptance failed')
                    fingerprint=git(worktree,'diff',entry['base'])
                    entry['state']='reviewing';save()
                    review=invoke(agent,worktree,attemptdir/'review',f"Read-only review. Review git diff against {entry['base']} for task {json.dumps(task)}. Inspect external acceptance.json and regression.log at {attemptdir}. Reject correctness gaps, weakened checks, sandbox bypasses, or unsupported parity claims. Do not edit or commit. Return status approved or rejected with findings.",True,state['deadline']);attempt['review']=review;check_trusted(state['base']);save()
                    if review['status']!='approved':raise RuntimeError('reviewer rejected: '+review['summary'])
                    # Recheck after reviewer, then stage an explicit set, never the whole directory.
                    if changed(worktree,entry['base'])!=files or git(worktree,'diff',entry['base'])!=fingerprint:raise RuntimeError('files changed during review')
                    entry['state']='committing';save()
                    git(worktree,'add','--',*files)
                    git(worktree,'commit','-m',f'parity: {ident}')
                    commit=git(worktree,'rev-parse','HEAD')
                    git(ROOT,'update-ref',f"refs/heads/{state['branch']}",commit,state['tip'])
                    state['tip']=commit;entry.update(state='verified_awaiting_review',commit=commit);attempt['state']='passing';save();break
                except StopRun:raise
                except Exception as error:
                    attempt.update(state='failed',reason=str(error));entry['state']='retrying';save()
            if entry['state']!='verified_awaiting_review':entry.update(state='blocked',reason=entry.get('reason','two implementation attempts exhausted'));save()
        state['status']='finished' if time.time()<state['deadline'] else 'deadline_exhausted';save()
    except (StopRun,KeyboardInterrupt) as error:
        state.update(status='stopped',reason=str(error));save()
    return state

def main():
    p=argparse.ArgumentParser();p.add_argument('command',choices=['run','resume','status']);p.add_argument('--base');p.add_argument('--run-id');p.add_argument('--agent',default='codex');p.add_argument('--paid',action='store_true');args=p.parse_args()
    if args.command!='status' and pathlib.Path(args.agent).name=='codex' and not args.paid:p.error('paid Codex execution requires explicit --paid')
    if args.run_id and (not args.run_id.replace('-','').replace('_','').isalnum()):p.error('invalid run ID')
    root=ROOT/'.parity/runs';root.mkdir(parents=True,exist_ok=True)
    if args.command=='run':
        if not args.base:p.error('run requires an explicit committed --base revision')
        base=git(ROOT,'rev-parse','--verify',args.base+'^{commit}')
        ident=args.run_id or time.strftime('%Y%m%d-%H%M%S')+'-'+uuid.uuid4().hex[:6]
        directory=root/ident;directory.mkdir()
        state={'schema_version':1,'id':ident,'branch':'automation/parity-'+ident,'base':base,'tip':base,'deadline':time.time()+DEADLINE,'processed':0,'tasks':{},'status':'running','backlog_hash':hashlib.sha256((ROOT/'parity/backlog.json').read_bytes()).hexdigest()}
        git(ROOT,'branch',state['branch'],base);atomic(directory/'state.json',state)
    else:
        if not args.run_id:p.error('--run-id is required')
        directory=root/args.run_id
    if args.command=='status':print(json.dumps(load(directory/'state.json'),indent=2));return
    if pathlib.Path(args.agent).name=='codex' and not args.paid:p.error('paid Codex execution requires explicit --paid; tests use --agent tools/fake_agent.py')
    agent=str(pathlib.Path(args.agent).resolve()) if '/' in args.agent else args.agent
    with open(directory/'lock','w') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        state=run_loop(directory,agent)
    print(json.dumps(state,indent=2))
    if state['status']!='finished' or any(t['state']=='blocked' for t in state['tasks'].values()):sys.exit(1)
if __name__=='__main__':main()
