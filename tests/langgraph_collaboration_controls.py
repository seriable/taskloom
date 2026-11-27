"""Scheduler controls only; fixture role outputs are never native evidence."""
from pathlib import Path
import sys
import threading
import unittest

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'examples/benchmarks'))
from langgraph_collaboration import build_graph


class FixtureProtocol:
    def __init__(self):
        self.calls=[];self.lock=threading.Lock();self.barrier=threading.Barrier(2)

    def role(self,case,node,role,*,dependencies=None,params=None):
        with self.lock:self.calls.append((node,role,dependencies,params))
        if role=='planner':
            return {'tasks':[{'id':name,'instructions':'fixture research'} for name in ['a','b']]}
        if role=='analyst':
            self.barrier.wait(timeout=5)
            return {'fixture':node}
        if role=='writer':
            assert len([v for v in dependencies.values() if isinstance(v,dict) and 'fixture' in v])==2
            if case.get('initial_draft'):assert dependencies['review-initial']['approved'] is False
            if params and params['revision']:
                assert dependencies['check-review-0']['approved'] is False
            return {'text':'fixture draft '+node,'citations':[]}
        return {'approved':node not in {'review-initial','review-0'},'feedback':'explicit scheduler fixture feedback'}


class GraphControls(unittest.TestCase):
    def test_parallel_join_submitted_review_and_bounded_revision(self):
        for kind,initial in [('dag',False),('dag',True),('peer',False)]:
            with self.subTest(kind=kind,initial=initial):
                case={'id':'scheduler-fixture','kind':kind,'branches':[{'id':name,'instructions':'fixture'} for name in ['a','b']]}
                if initial:case['initial_draft']={'text':'bad fixture','citations':[]}
                protocol=FixtureProtocol()
                outputs=build_graph(case,protocol).invoke({'outputs':{}},{'max_concurrency':3 if initial else 2,'recursion_limit':32})['outputs']
                self.assertEqual(len([c for c in protocol.calls if c[1]=='analyst']),2)
                if kind=='peer':
                    self.assertEqual([c[0] for c in protocol.calls if c[1]=='writer'],['draft-0','draft-1'])
                    self.assertTrue(outputs['review-1']['approved'])
                    self.assertIsNone(outputs['plan']['tasks'][0]['input'])
                else:self.assertTrue(outputs['review']['approved'])


if __name__=='__main__':unittest.main()
