import copy,json,pathlib,unittest
from unittest.mock import patch
import parity,reference,vdj
class ContractTests(unittest.TestCase):
    def test_meaningful_value_selection_order_and_type_mutations_fail(self):
        base={'sheets':[{'alias':'rows','columns':['name'],'rows':[{'ref':'1','cells':[{'Text':'α'}],'selected':False},{'ref':'2','cells':[{'Text':''}],'selected':True}]}]}
        for mutate in [lambda x:x['sheets'][0]['rows'][0]['cells'].__setitem__(0,{'Text':'β'}),lambda x:x['sheets'][0]['rows'][0].__setitem__('selected',True),lambda x:x['sheets'][0]['rows'].reverse(),lambda x:x['sheets'][0]['rows'][1]['cells'].__setitem__(0,'Null')]:
            actual=copy.deepcopy(base);mutate(actual);diff=parity.compare(base,actual);self.assertIsNotNone(diff);self.assertIn('sheets[0].rows',diff['path'])
    def test_reference_mismatch_is_infrastructure_failure(self):
        with patch.object(parity,'check',return_value='unused'),patch.object(parity,'metadata',return_value={'wrong':'version'}):
            result=parity.verify(['existing-edit'])
        self.assertEqual(result['results'][0]['status'],'infrastructure_failure')
    def test_vdj_strict_subset_roundtrip_and_rejections(self):
        native=[{'kind':'header','schema_version':1},{'kind':'command','name':'invert_selection','inputs':[]}]
        self.assertEqual(vdj.decode([json.dumps(r) for r in vdj.encode(native)]),native)
        for row in [{'longname':'unknown'},{'longname':'stoggle-rows','input':'unsupported'},{'longname':'stoggle-rows','row':'1'}]:
            with self.assertRaises(ValueError):vdj.decode([json.dumps(row)])
    def test_backlog_existing_features_are_never_implicitly_passing(self):
        data=parity.load(parity.ROOT/'parity/backlog.json')
        for task in data['tasks']:
            self.assertIn(task['verification_state'],['unverified','deferred'])
            if task['design_decisions']:self.assertFalse(task['execution_ready'])
            for key in ['id','scope','dependencies','upstream_evidence','acceptance_scenarios']:self.assertIn(key,task)
