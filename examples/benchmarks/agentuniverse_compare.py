"""Run the real agentUniverse PEER and parallel executor with native role I/O.

Qualified comparison: shared prompts/model/task/corpus/output ceilings and
read-only evidence validation; real upstream PEER retry control and executor
ThreadPool. Native role adapters replace model/knowledge I/O only. No patched
framework source, scripted model answers, fake crash/approval, or speed ranking.
"""
import argparse
import hashlib
import importlib.metadata
import inspect
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from typing import ClassVar

for name in ['LANGSMITH_TRACING', 'LANGCHAIN_TRACING', 'LANGCHAIN_TRACING_V2']:
    os.environ[name] = 'false'
os.environ['OTEL_SDK_DISABLED'] = 'true'
os.environ['ANONYMIZED_TELEMETRY'] = 'false'

from agentuniverse.agent.input_object import InputObject
from agentuniverse.agent.output_object import OutputObject
from agentuniverse.agent.template.planning_agent_template import PlanningAgentTemplate
from agentuniverse.agent.template.executing_agent_template import ExecutingAgentTemplate
from agentuniverse.agent.template.expressing_agent_template import ExpressingAgentTemplate
from agentuniverse.agent.template.reviewing_agent_template import ReviewingAgentTemplate
from agentuniverse.agent.work_pattern.peer_work_pattern import PeerWorkPattern

from native_retrieval import host, save
from native_role_protocol import NativeRoles


class NativePlanning(PlanningAgentTemplate):
    protocol: ClassVar[NativeRoles]
    case: ClassVar[dict]

    def run(self, **kwargs):
        plan = self.protocol.role(self.case, 'raw-plan', 'planner')
        # The framework's framework array is its task carrier. Preserve each
        # complete native plan task; do not substitute predefined assignments.
        return OutputObject({'framework': plan['tasks'], 'thought': ''})


class NativeExecuting(ExecutingAgentTemplate):
    protocol: ClassVar[NativeRoles]
    case: ClassVar[dict]

    def run(self, **kwargs):
        tasks = kwargs['planning_result'].get_data('framework')
        if not 1 <= len(tasks) <= 2 or len({t['id'] for t in tasks}) != len(tasks):
            raise ValueError('native plan is outside the shared two-task bound')
        # Upstream owns ThreadPool creation, context propagation, submit order
        # and collection. Only its subtask model/knowledge I/O hook is adapted.
        result = self._execute_tasks(InputObject(kwargs), {'framework': tasks}, None, None, None)
        return OutputObject(result)

    def _execute_subtask(self, subtask, input_object, agent_input, index, memory, llm, prompt, **kwargs):
        output = self.protocol.role(self.case, 'worker-' + str(index), 'analyst',
                                    dependencies={'task': subtask}, params={'index': index})
        return {'index': index, 'input': subtask, 'output': output}


class NativeExpressing(ExpressingAgentTemplate):
    protocol: ClassVar[NativeRoles]
    case: ClassVar[dict]
    revision: ClassVar[int] = 0

    def run(self, **kwargs):
        revision = type(self).revision
        dependencies = {'worker-' + str(t['index']): t['output']
                        for t in kwargs['executing_result'].get_data('executing_result')}
        dependencies['plan'] = {'tasks': kwargs['planning_result'].get_data('framework')}
        if revision:
            dependencies['draft-' + str(revision - 1)] = kwargs['expressing_result'].get_data('output')
            review = kwargs['reviewing_result'].get_data('native_review')
            dependencies['check-review-' + str(revision - 1)] = review
        draft = self.protocol.role(self.case, 'draft-' + str(revision), 'writer',
                                   dependencies=dependencies, params={'revision': revision})
        type(self).revision += 1
        return OutputObject({'output': draft})


class NativeReviewing(ReviewingAgentTemplate):
    protocol: ClassVar[NativeRoles]
    case: ClassVar[dict]

    def run(self, **kwargs):
        revision = NativeExpressing.revision - 1
        review = self.protocol.role(self.case, 'review-' + str(revision), 'reviewer',
            dependencies={'draft': kwargs['expressing_result'].get_data('output')}, params={'revision': revision})
        return OutputObject({'score': 100 if review['approved'] else 0, 'suggestion': review['feedback'],
                             'native_review': review})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('reference', type=Path)
    parser.add_argument('out', type=Path)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--verifier', type=Path, required=True)
    parser.add_argument('--source', type=Path, required=True)
    args = parser.parse_args()
    reference = json.loads((args.reference / 'report.json').read_text(encoding='utf-8'))
    suite = json.loads((args.reference / 'suite.json').read_text(encoding='utf-8'))
    if not reference['completed'] or suite['version'] != 5:
        raise ValueError('a completed exact version-5 native reference is required')
    directory = args.out.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    for name in ['agentuniverse_compare.py','native_role_protocol.py','native_retrieval.py']:
        shutil.copy2(Path(__file__).with_name(name), directory / name)
    config = dict(reference['configuration'], state_dir=str(directory/'state'), roles=[], skills=[], memory=None, mcp=[],
                  provider={'type':'offline'}, prices=[], observability=None)
    save(directory/'taskloom.json', config)
    report = {'version': 1, 'framework': 'agentUniverse', 'framework_version': importlib.metadata.version('agentUniverse'),
              'execution_provenance': {'taskloom_binary_sha256':hashlib.sha256(args.taskloom.read_bytes()).hexdigest(),
                  'verifier_binary_sha256':hashlib.sha256(args.verifier.read_bytes()).hexdigest(),
                  'harness_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  'role_adapter_sha256':hashlib.sha256(Path(__file__).with_name('native_role_protocol.py').read_bytes()).hexdigest()},
              'source_commit': subprocess.check_output(['git','-C',str(args.source.resolve()),'rev-parse','HEAD'],text=True).strip(),
              'reference_suite_sha256': reference['suite_sha256'], 'cases': [], 'completed': False,
              'evidence_kind': 'actual upstream PeerWorkPattern + ExecutingAgentTemplate + actual local native role API',
              'fees': {'currency':'USD','input_per_million':0,'output_per_million':0,'campaign_cap_micros':1,
                       'model_credential_environment':reference['configuration']['provider']['key_env'],
                       'excluded':'hardware/electricity/current-chat usage; no embedding or tool-service charges'},
              'conditions': ['same model, tasks, role prompts, frozen knowledge tool, corpus, max output tokens, per-role 16 decisions/32 requested tools and 300-second case cap',
                  'real upstream PEER threshold loop: retry_count=2, jump_step=expressing, eval_threshold=60; boolean verdict maps to score 0/100',
                  'shared read-only knowledge uses Taskloom retrieval; this does not compare native framework retrieval quality'],
              'differences': ['model/knowledge I/O uses explicit harness adapters, not default agentUniverse provider/ReAct/memory',
                  'upstream parallel executor delays each submission by one second; Taskloom does not',
                  'role repair feedback is bounded generic text; Taskloom uses SDK-specific source-locator diagnostics',
                  'native plan task objects are carried directly; Taskloom typed planning also materializes absent optional input fields as null',
                  'this native transport makes one HTTP attempt per decision; the Taskloom gateway permits transient retries',
                  'framework prompt/wrapper/repair differences prohibit identical-conditioning or speed/leadership claims'],
              'not_comparable': [{'case':c['id'],'reason':'this PEER adapter does not implement arbitrary DAGs or the externally submitted initial-draft-review graph'}
                                 for c in suite['cases'] if c['kind'] != 'peer'],
              'unverified': ['process-death durability, exact approvals, uncertain effect reconciliation, long tasks, independent claim support'],
              'installed_distribution_manifest': subprocess.check_output([os.sys.executable,'-m','pip','freeze'],text=True).splitlines()}
    report['upstream_control_code'] = []
    for cls, relative in [(PeerWorkPattern, 'agentuniverse/agent/work_pattern/peer_work_pattern.py'),
                          (ExecutingAgentTemplate, 'agentuniverse/agent/template/executing_agent_template.py')]:
        installed = Path(inspect.getfile(cls))
        checkout = args.source.resolve()/relative
        a, b = hashlib.sha256(installed.read_bytes()).hexdigest(), hashlib.sha256(checkout.read_bytes()).hexdigest()
        if a != b:
            raise ValueError('installed control code differs from the pinned source checkout: '+relative)
        report['upstream_control_code'].append({'class':cls.__name__,'relative_path':relative,'sha256':a,'matches_checkout':True})
    save(directory/'report.json', report)
    with host(args.taskloom.resolve(), directory/'taskloom.json', directory) as api:
        for i, document in enumerate(suite['documents']):
            api('/knowledge', {'expected_revision':i,'document':document})
        for case in suite['cases']:
            if case['kind'] != 'peer':
                continue
            case_root = directory/case['id']
            case_root.mkdir()
            protocol = NativeRoles(reference['configuration'], suite, case_root, api, args.verifier.resolve(), reference)
            for cls in [NativePlanning, NativeExecuting, NativeExpressing, NativeReviewing]:
                cls.protocol, cls.case = protocol, case
            NativeExpressing.revision = 0
            result = {'id':case['id'],'failures':[]}
            started = time.monotonic()
            try:
                pattern = PeerWorkPattern()
                pattern.planning, pattern.executing = NativePlanning(), NativeExecuting()
                pattern.expressing, pattern.reviewing = NativeExpressing(), NativeReviewing()
                rounds = pattern.invoke(InputObject({'input':case['goal']}),
                    {'input':case['goal'],'retry_count':2,'jump_step':'expressing','eval_threshold':60})
                result['rounds'] = rounds
                last = rounds['result'][-1]
                final = last['expressing_result']['output']
                result['final_output'] = final
                if not last['reviewing_result']['native_review']['approved']:
                    result['failures'].append('native reviewer did not approve within one revision')
                for phrase in case['required_phrases']:
                    if phrase.lower() not in final['text'].lower():
                        result['failures'].append('missing requested topic: '+phrase)
                # Per-role SDK validation already resolved exact locators;
                # final source coverage is assessed separately from prose.
                evidence = [m['result']['output'] for r in protocol.records if r['role']=='writer' and r['status']=='completed'
                            for m in r['messages'] if m['role']=='tool']
                checked = subprocess.run([str(args.verifier.resolve())],input=json.dumps({'report':final,'evidence':evidence}).encode(),capture_output=True,timeout=10)
                if checked.returncode:
                    raise RuntimeError(checked.stderr.decode(errors='replace'))
                documents = {s['document_id'] for s in json.loads(checked.stdout)['sources']}
                if not set(case['required_documents']) <= documents:
                    result['failures'].append('required source coverage missing')
            except Exception as error:
                result['failures'].append(str(error))
            result.update(status='failed' if result['failures'] else 'passed', wall_ms=round((time.monotonic()-started)*1000),
                          maximum_concurrent_analysts=protocol.maximum_concurrent_analysts,
                          roles=[{'node':r['node'],'status':r['status'],'usage':r['usage'],
                                  'native_usage_complete':r['native_usage_complete'],'http_attempt_records':len(r['requests']),
                                  'repairs':r['repairs'],'failure':r.get('failure')} for r in protocol.records])
            if case['require_parallel'] and protocol.maximum_concurrent_analysts < 2:
                result['failures'].append('two real analyst executions did not overlap')
                result['status']='failed'
            save(case_root/'result.json',result)
            report['cases'].append(result)
            save(directory/'report.json',report)
            print(json.dumps(result,ensure_ascii=False),flush=True)
    report.update(completed=True,passed=sum(c['status']=='passed' for c in report['cases']),failed=sum(c['status']=='failed' for c in report['cases']))
    save(directory/'report.json',report)
    raise SystemExit(1 if report['failed'] else 0)


if __name__=='__main__':
    main()
