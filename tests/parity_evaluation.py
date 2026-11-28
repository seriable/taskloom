"""Offline evaluator controls; fixtures below are not actual human labels."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('parity_module', ROOT/'examples/benchmarks/parity_evaluation.py')
parity = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(parity)


def manifest(name='fixture-reference'):
    conditions = {key: 'a'*64 for key in parity.CONDITION_KEYS}
    conditions.update(model={'provider':'fixture','endpoint_identity':'loopback-fixture','model':'fixture-model','parameters':{'temperature':0.0,'thinking':False}},
        tasks=['a','b'],output_tokens=512,max_http_attempts_per_case=8,
        per_case_budget={'currency':'USD','max_cost_micros':100,'prices_sha256':'b'*64},
        deadline_seconds=60,retry_policy={'upstream_retries':0,'output_repairs':1})
    return {'version':1,'framework':{'name':name,'version':'fixture-v1','source_identity':'fixture-not-real-framework'},'conditions':conditions}


def annotations():
    # Explicit unit fixture; this does not assert a person supplied annotations.
    return {'version':1,'kind':'external_human_annotations',
        'author':{'id':'UNIT-FIXTURE-NOT-A-PERSON','kind':'human','independent':True,'affiliation':'unit fixture','declaration':'Synthetic schema control; not an actual annotation'},
        'cases':[{'id':'a','author':'UNIT-FIXTURE-NOT-A-PERSON','output_sha256':hashlib.sha256(b'actual output').hexdigest(),
                  'coverage':{'exact_provenance':'verified','source_support':'insufficient','corpus_factuality':'supported','task_coverage':'partial'},
                  'conflict':{'status':'none','note':''},'reason':'Fixture tests separation, no factual claim.'}]}


class ParityControls(unittest.TestCase):
    def test_pairwise_agreement_reports_kappa_and_keeps_exclusions_visible(self):
        first, second = annotations(), annotations()
        second['author']['id']='OTHER-UNIT-FIXTURE-NOT-A-PERSON'
        rows=[]
        for identity in ['a','b','c','d']:
            row=copy.deepcopy(first['cases'][0]);row['id']=identity;rows.append(row)
        first['cases']=rows
        second['cases']=copy.deepcopy(rows)
        for row in second['cases']:row['author']=second['author']['id']
        for document, values in [(first,['supported','supported','contradicted','contradicted']),
                                 (second,['supported','contradicted','contradicted','supported'])]:
            for row,label in zip(document['cases'],values):row['coverage']['source_support']=label
        result=parity.annotation_agreement(['a','b','c','d','absent'],[first,second])
        score=result['pairs'][0]['dimensions']['source_support']
        self.assertEqual(score['paired_valid'],4);self.assertEqual(score['coverage'],.8)
        self.assertEqual(score['observed_agreement'],.5);self.assertEqual(score['cohen_kappa'],0)
        self.assertEqual(score['excluded'],{'missing_annotation':1})
        self.assertIsNone(result['pairs'][0]['dimensions']['exact_provenance']['cohen_kappa'])
        self.assertFalse(result['is_correctness_score']);self.assertFalse(result['human_identity_verified'])
        second['cases'][0]['conflict']['status']='open'
        second['cases'][1]['coverage']['source_support']=None
        second['cases'][2]['coverage']['source_support']='error'
        score=parity.annotation_agreement(['a','b','c','d','absent'],[first,second])['pairs'][0]['dimensions']['source_support']
        self.assertEqual(score['paired_valid'],1)
        self.assertEqual(score['excluded'],{'open_conflict':1,'missing_label':1,'annotation_error':1,'missing_annotation':1})

    def test_agreement_refuses_duplicate_raters_and_different_answer_hashes(self):
        first,second=annotations(),annotations()
        with self.assertRaises(ValueError):parity.annotation_agreement(['a'],[first,second])
        second['author']['id']='OTHER-UNIT-FIXTURE-NOT-A-PERSON'
        second['cases'][0]['author']=second['author']['id'];second['cases'][0]['output_sha256']='f'*64
        with self.assertRaises(ValueError):parity.annotation_agreement(['a'],[first,second])
        first['cases'][0]['output_sha256']='z'*64
        with self.assertRaises(ValueError):parity.validate_annotations(first,{'a'})

    def test_all_conditions_must_match_including_http_and_prices(self):
        reference=manifest()
        other=manifest('fixture-candidate')
        self.assertEqual(parity.compare_manifests(reference,other)['status'],'declared_conditions_equal')
        for key,value in [('output_tokens',1024),('max_http_attempts_per_case',9),('deadline_seconds',61),('tasks',['b','a']),('system_prompts_sha256','c'*64)]:
            changed=copy.deepcopy(other);changed['conditions'][key]=value
            result=parity.compare_manifests(reference,changed)
            self.assertEqual(result['status'],'not_comparable')
            self.assertEqual(result['differences'],[key])
            self.assertFalse(result['performance_rank_allowed'])
        changed=copy.deepcopy(other);changed['conditions']['per_case_budget']['prices_sha256']='c'*64
        self.assertEqual(parity.compare_manifests(reference,changed)['status'],'not_comparable')

    def test_missing_defaults_and_bool_int_parameter_collisions_refuse_parity(self):
        bad=manifest();del bad['conditions']['output_schema_sha256']
        with self.assertRaises(ValueError):parity.validate_manifest(bad)
        bad=manifest();bad['conditions']['max_http_attempts_per_case']=True
        with self.assertRaises(ValueError):parity.validate_manifest(bad)
        other=manifest();other['conditions']['model']['parameters']['thinking']=0
        self.assertEqual(parity.compare_manifests(manifest(),other)['status'],'not_comparable')

    def test_annotations_do_not_attest_human_identity_or_accept_agent_authors(self):
        result=parity.validate_annotations(annotations(),{'a','b'})
        self.assertEqual(result['missing'],['b'])
        self.assertFalse(result['human_identity_verified'])
        self.assertFalse(result['independence_verified'])
        for kind in ['agent','model','codex']:
            bad=annotations();bad['author']['kind']=kind
            with self.assertRaises(ValueError):parity.validate_annotations(bad,{'a'})
        bad=annotations();bad['cases'][0]['author']='someone-else'
        with self.assertRaises(ValueError):parity.validate_annotations(bad,{'a'})

    def test_separate_dimensions_keep_missing_errors_and_conflicts_in_denominator(self):
        result=parity.score_answers(['a','b','c'],[{'id':'a','text':'actual output'},{'id':'b','error':'timeout'}],annotations())
        self.assertEqual(result['dimensions']['exact_provenance'],{'passed':1,'denominator':3,'rate':1/3})
        self.assertEqual(result['dimensions']['source_support']['passed'],0)
        self.assertEqual(result['dimensions']['corpus_factuality']['passed'],1)
        self.assertEqual(result['dimensions']['task_coverage']['passed'],0)
        conflict=annotations();conflict['cases'][0]['conflict']['status']='open'
        self.assertEqual(parity.score_answers(['a'],[{'id':'a','text':'actual output'}],conflict)['dimensions']['exact_provenance']['passed'],0)
        bad=annotations()
        with self.assertRaises(ValueError):parity.score_answers(['a'],[{'id':'a','text':'modified output'}],bad)

    def test_retrieval_errors_do_not_turn_into_correct_empty_negatives(self):
        suite={'queries':[{'id':'positive','text':'x','groups':['short'],'relevant':['doc']},
                          {'id':'negative','text':'y','groups':['negative'],'relevant':[]}]}
        result=parity.score_retrieval(suite,[{'id':'negative','error':'unavailable'}])
        self.assertEqual(result['all']['cases'],2)
        self.assertEqual(result['all']['errors_or_missing'],2)
        self.assertEqual(result['all']['negative_empty'],0)
        self.assertEqual(result['all']['recall_at_5'],0)

    def test_model_input_allowlist_strips_all_expected_labels(self):
        suite={'queries':[{'id':'a','text':'question','relevant':['gold'],'groups':['chinese'],
                           'expected_source_support':'supported','label_author':'private labeler'}]}
        self.assertEqual(parity.model_inputs(suite),[{'id':'a','query':'question'}])

    def test_actual_suite_source_bindings_and_all_groups(self):
        suite=parity.read(ROOT/'examples/benchmarks/parity-evaluation-v1.json')
        parity.validate_suite(suite,ROOT)
        dataset=parity.retrieval_dataset(suite,ROOT)
        self.assertEqual(len(dataset['queries']),28)
        self.assertEqual(len(dataset['documents']),8)
        altered=copy.deepcopy(suite);altered['documents'][0]['text']+=' drift'
        with self.assertRaises(ValueError):parity.validate_suite(altered,ROOT)
        with tempfile.TemporaryDirectory() as temp:
            path=Path(temp)/'output.json';parity.create(path,dataset)
            with self.assertRaises(FileExistsError):parity.create(path,dataset)


if __name__=='__main__':unittest.main()
