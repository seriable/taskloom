"""Actual local native SQL corpus/memory acceptance; no paid endpoints.

Retains model extraction separately from labelled operator review/edit/erasure.
Uses an explicitly owned loopback PostgreSQL fixture. No identity/RBAC, vector
quality, service queue, whole-app migration or overall P2 completion claim.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from urllib.parse import quote, urlsplit
import uuid


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--example', type=Path, required=True)
    parser.add_argument('--postgres-fixture', type=Path, required=True)
    parser.add_argument('--model-log', type=Path, required=True)
    args=parser.parse_args()
    cfg=json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1','localhost','::1'}
        or not cfg.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in cfg['prices'])):
        raise ValueError('explicit zero-priced loopback native configuration required')
    if not os.environ.get(cfg['provider']['key_env']):
        raise ValueError('configure the named local model credential environment first')
    public=json.loads((args.postgres_fixture/'public.json').read_text())
    credentials=json.loads((args.postgres_fixture/'credentials.json').read_text())
    pg=credentials['TASKLOOM_TEST_POSTGRES_URL']
    parsed=urlsplit(pg)
    if parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:
        raise ValueError('database must match owned loopback fixture')
    root=args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    cfg['output_tokens']=256
    save(root/'native.json', cfg)
    shutil.copy2(__file__, root/'harness.py')
    report={'version':1,'completed':False,'cases':[],'failures':[],
        'scope':'native SQL corpus/memory persistence and cold reads; explicit host review/edit/erasure, not native semantic/authorization benchmark',
        'binary_sha256':hashlib.sha256(args.example.read_bytes()).hexdigest(),
        'postgres_public_provenance':public,
        'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'cap_micros':0,
            'output_allowance':256,'model_credential_environment':cfg['provider']['key_env'],
            'database_credential_environment':'TASKLOOM_DATABASE_URL',
            'excluded':'hardware/electricity/current chat; no embedding/tool-service calls'},
        'limits':['Host-authored corpus and visible short preference; no held-out semantic/factual extraction or retrieval benchmark.',
            'Local host operator review/edit/erasure is labelled, never attributed to the native model.',
            'Namespaces are trusted SDK input, not authenticated HTTP isolation; app backend and service integration remain unfinished.',
            'No SQL skill catalog/embedding ledger or whole-app backup/import. Earlier core-SQL backup acceptance covers its earlier tables/scope only.']}
    baseline=lambda:args.model_log.read_bytes().count(b'[Request received] Model:')
    started=time.monotonic()
    for backend,url in {'sqlite':'sqlite://'+quote(str(root/'knowledge ü.sqlite'),safe='')+'?mode=rwc&cache=private','postgres':pg}.items():
        out=root/backend
        out.mkdir()
        project='knowledge-'+uuid.uuid4().hex
        tenant='native-knowledge'
        environment=dict(os.environ, TASKLOOM_DATABASE_URL=url)
        result={'backend':backend,'project':project,'checks':{},'failures':[]}
        def run(action, cold=False, other_project=None):
            env=dict(environment)
            if cold:env.pop(cfg['provider']['key_env'],None)
            cmd=[str(args.example.resolve()),action,tenant,other_project or project]
            if action=='knowledge-run':cmd.append(str(root/'native.json'))
            value=subprocess.run(cmd, env=env, capture_output=True, timeout=180)
            # Always keep partial stdout to retain native failures and their outputs.
            (out/(action+('-cold' if cold else '')+'.jsonl')).write_bytes(value.stdout)
            if value.returncode:
                raise RuntimeError(action+' failed: '+value.stderr.decode(errors='replace')[:1500])
            return [json.loads(line) for line in value.stdout.decode('utf-8').splitlines()]
        try:
            run('init', cold=True)
            before=baseline()
            extracted,operator,snapshot=run('knowledge-run')
            native=extracted['result']
            checks=result['checks']
            checks['native_extraction_completed_with_exact_source']=native['run']['report']['stop_reason']=='completed' and all(r['source']['quote'] in snapshot['session']['turns'][0]['binding']['input'] for r in native['memory']['records'].values())
            checks['native_candidate_persisted_before_host_review']=native['proposals_recorded'] and len(native['memory']['records'])>=1 and all(r['status']=='candidate' for r in native['memory']['records'].values())
            active=[r for r in operator['active']['records'].values() if r['status']=='active']
            checks['explicit_operator_review_has_actor_and_source']=len(active)==1 and active[0]['decision']['actor']=='local-operator'
            checks['other_owner_empty_while_alice_live']=not operator['other_owner_while_live']['records']
            checks['operator_edit_is_distinct_provenance']=any(r['status']=='active' and r['origin']['type']=='operator_edit' for r in operator['edited']['records'].values())
            checks['erasure_tombstone_prevents_source_replay']=not operator['erased']['records'] and operator['erased']['forgotten_sources'] and operator['erased']==operator['replay']
            checks['sql_corpus_retrieves_title_and_exact_unicode_body']=snapshot['retrieval']['hits'] and 'Unicode 原文' in snapshot['retrieval']['hits'][0]['chunk']['text']
            cold=run('knowledge-inspect', cold=True)[0]
            save(out/'same-scope-cold.json', cold)
            checks['cold_separate_process_matches_native_final_state']=cold==snapshot
            other=run('knowledge-inspect', cold=True, other_project='foreign-'+uuid.uuid4().hex)[0]
            save(out/'other-project-cold.json',other)
            checks['other_project_cannot_read_source_corpus_or_memory']=not other['corpus']['documents'] and not other['memory']['records'] and other['session'] is None
            checks['cold_inspection_did_not_call_native_api']=baseline()==before+2
            result['native_usage']={key:sum(r['report']['usage'][key] for r in [snapshot['source_run'],native['run']])
                for key in ['requests','input_tokens','output_tokens','cost_micros']}
            result['native_attempts_observed']=baseline()-before
        except Exception as error:
            result['failures'].append(str(error))
        result['failures'].extend('check failed: '+name for name,passed in result['checks'].items() if not passed)
        result['status']='failed' if result['failures'] else 'passed'
        save(out/'report.json',result)
        report['cases'].append(result)
        report['failures'].extend(backend+': '+reason for reason in result['failures'])
    report['completed']=True
    report['status']='failed' if report['failures'] else 'passed'
    report['wall_ms']=round((time.monotonic()-started)*1000)
    save(root/'report.json',report)
    print(json.dumps(report, ensure_ascii=False))
    raise SystemExit(1 if report['failures'] else 0)


if __name__=='__main__':
    main()
