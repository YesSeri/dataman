import json,os,pathlib,subprocess,tempfile,time,unittest
from unittest.mock import patch
import runner
from runner import atomic,git,process,StopRun

class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=pathlib.Path(self.temp.name)
        git(self.root,'init','-q');git(self.root,'config','user.email','test@example.invalid');git(self.root,'config','user.name','Runner Test')
        (self.root/'parity').mkdir();(self.root/'implementation.txt').write_text('pending\n')
        self.tasks=[dict(id='test-task',scope='test',execution_ready=True,design_decisions=[],dependencies=[],acceptance_scenarios=['test'])]
        atomic(self.root/'parity/backlog.json',dict(tasks=self.tasks))
        git(self.root,'add','.');git(self.root,'commit','-qm','base');self.base=git(self.root,'rev-parse','HEAD')
        self.directory=self.root/'.parity/run';self.directory.mkdir(parents=True)
        self.state=dict(schema_version=1,base=self.base,tip=self.base,branch='automation/test',deadline=time.time()+60,processed=0,tasks={},status='running',backlog_hash=runner.hashlib.sha256((self.root/'parity/backlog.json').read_bytes()).hexdigest())
        atomic(self.directory/'state.json',self.state);git(self.root,'branch','automation/test',self.base)
        self.agent=str(pathlib.Path(__file__).with_name('fake_agent.py').resolve())
    def tearDown(self):self.temp.cleanup()
    def acceptance(self,task,worktree,directory,deadline):
        filename='implementation.txt' if task['id']=='test-task' else f"implementation-{task['id']}.txt"
        passed=(worktree/filename).read_text()=='implemented\n'
        atomic(directory/'acceptance.json',{'results':[{'status':'passing' if passed else 'failing'}]})
        (directory/'regression.log').write_text('fake regression passed')
        return passed
    def execute(self,mode='complete'):
        with patch.object(runner,'ROOT',self.root),patch.object(runner,'verify_task',self.acceptance),patch.dict(os.environ,{'PARITY_FAKE_MODE':mode}):
            return runner.run_loop(self.directory,self.agent)
    def test_fake_agent_complete_run_resumes_without_duplicate_commit_and_preserves_dirty_user_tree(self):
        (self.root/'implementation.txt').write_text('unrelated dirty user change\n')
        state=self.execute();task=state['tasks']['test-task'];self.assertEqual(task['state'],'verified_awaiting_review')
        self.assertEqual((self.root/'implementation.txt').read_text(),'unrelated dirty user change\n')
        tip=state['tip'];self.assertNotEqual(tip,self.base);self.assertEqual(git(self.root,'rev-parse','HEAD'),self.base)
        state=self.execute();self.assertEqual(tip,state['tip']);self.assertEqual(len(task['attempts']),1)
        self.assertTrue((self.directory/'test-task-attempt-1/review/events.jsonl').exists())
    def test_protected_file_changes_are_rejected_and_retry_exhausts(self):
        state=self.execute('protected');task=state['tasks']['test-task'];self.assertEqual(task['state'],'blocked')
        self.assertEqual(len(task['attempts']),2);self.assertIn('protected-file',task['attempts'][0]['reason']);self.assertEqual(state['tip'],self.base)
    def test_reviewer_rejection_never_advances_branch(self):
        state=self.execute('reject');self.assertEqual(state['tasks']['test-task']['state'],'blocked');self.assertEqual(state['tip'],self.base)
    def test_authentication_stops_run(self):
        state=self.execute('auth');self.assertEqual(state['status'],'stopped');self.assertIn('authentication',state['reason'])
    def test_interrupt_resume_preserves_attempt_and_finishes(self):
        original=runner.invoke
        def interrupt(*args,**kwargs):raise StopRun('interrupted')
        with patch.object(runner,'invoke',interrupt):state=self.execute()
        self.assertEqual(state['status'],'stopped');self.assertEqual(len(state['tasks']['test-task']['attempts']),1)
        self.assertEqual(self.execute()['tasks']['test-task']['state'],'verified_awaiting_review')
    def test_scheduler_dependencies_design_decisions_and_blocked_independent_tasks(self):
        backlog=dict(tasks=[dict(id='dep',execution_ready=True,design_decisions=[],dependencies=['a']),dict(id='decision',execution_ready=True,design_decisions=['choose'],dependencies=[]),dict(id='independent',execution_ready=True,design_decisions=[],dependencies=[])])
        state=dict(tasks={'a':dict(state='blocked')})
        self.assertEqual(runner.select_ready(backlog,state)['id'],'independent')
        state['tasks']['a']['state']='verified_awaiting_review';self.assertEqual(runner.select_ready(backlog,state)['id'],'dep')
    def test_timeout_terminates_subprocess_group(self):
        with self.assertRaises(StopRun):process(['python3','-c','import time; time.sleep(30)'],self.root,self.root/'timeout.log',time.time()+0.1)
    def test_protected_path_policy(self):
        for name in ['parity/references/x','parity/scenarios.json','tools/parity.py','tools/runner.py','parity/backlog.json','tests/acceptance.rs']:
            self.assertTrue(runner.protected(name))
        self.assertFalse(runner.protected('src/controller/controller_impl.rs'))
    def test_deadline_is_not_reset_on_resume(self):
        self.state['deadline']=time.time()-1;atomic(self.directory/'state.json',self.state)
        self.assertEqual(self.execute()['status'],'deadline_exhausted')

    def test_three_task_limit_and_tasks_depend_on_prior_commits(self):
        tasks=[]
        for index,ident in enumerate(['test-task','second','third','fourth']):
            tasks.append(dict(id=ident,scope='test',execution_ready=True,design_decisions=[],dependencies=['test-task'] if index==1 else [],acceptance_scenarios=['test']))
            if index:(self.root/f'implementation-{ident}.txt').write_text('pending\n')
        atomic(self.root/'parity/backlog.json',dict(tasks=tasks));git(self.root,'add','.');git(self.root,'commit','-qm','contracts')
        base=git(self.root,'rev-parse','HEAD');git(self.root,'branch','-f','automation/test',base)
        self.state.update(base=base,tip=base,backlog_hash=runner.hashlib.sha256((self.root/'parity/backlog.json').read_bytes()).hexdigest());atomic(self.directory/'state.json',self.state)
        state=self.execute();self.assertEqual(state['processed'],3);self.assertEqual(len(state['tasks']),3)
        self.assertTrue(all(t['state']=='verified_awaiting_review' for t in state['tasks'].values()))
        self.assertEqual(state['tasks']['second']['base'],state['tasks']['test-task']['commit'])
