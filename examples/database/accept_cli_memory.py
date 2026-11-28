"""Native SQL CLI extraction followed immediately by cold terminal reentry.

Two short actual generations per backend; zero-priced loopback only. This is
release/idempotence evidence, not broad memory extraction/semantic quality.
"""
import argparse,hashlib,json,os,subprocess,sys,time,uuid
from pathlib import Path
from urllib.parse import quote,urlsplit
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'benchmarks'))
from native_retrieval import host,save

def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('out',type=Path);p.add_argument('--native',type=Path,required=True)
    p.add_argument('--taskloom',type=Path,required=True);p.add_argument('--postgres-fixture',type=Path,required=True);p.add_argument('--model-log',type=Path,required=True)
    a=p.parse_args();root=a.out.resolve();root.mkdir(parents=True,exist_ok=False);exe=a.taskloom.resolve()
    cfg=json.loads((a.native/'sqlite/app.json').read_text(encoding='utf-8'))
    if urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1','localhost','::1'} or not cfg.get('prices') or any(v['input_per_million'] or v['output_per_million'] for v in cfg['prices']):raise ValueError('explicit zero-priced loopback configuration required')
    public=json.loads((a.postgres_fixture/'public.json').read_text());pg=json.loads((a.postgres_fixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    parsed=urlsplit(pg)
    if parsed.hostname!='127.0.0.1' or parsed.port!=public['loopback_port']:raise ValueError('owned loopback PostgreSQL required')
    report={'version':1,'completed':False,'cases':[],'failures':[],'binary_sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),
        'scope':'actual native source turn and CLI extraction; immediate separate-process repeated extraction with ten-second SQL TTL',
        'fees':{'currency':'USD','input_per_million':0,'output_per_million':0,'cap_micros':0,'output_allowance':256,'credential_environment':cfg['provider']['key_env'],'excluded':'hardware/electricity/current chat; no embedding/tool services'},
        'limits':['Visible short preference, no held-out quality, multi-user private memory or service-worker/load acceptance.']}
    requests=lambda:a.model_log.read_bytes().count(b'[Request received] Model:')
    for backend,url in {'sqlite':'sqlite://'+quote(str(root/'cli.sqlite'),safe='')+'?mode=rwc&cache=private','postgres':pg}.items():
        out=root/backend;out.mkdir();os.environ['TASKLOOM_DATABASE_URL']=url
        config=json.loads(json.dumps(cfg));project='cli-'+uuid.uuid4().hex;config['database']['project']=project;config['database']['lease_ms']=10000
        config['memory']['scope']['project']=project;config['skills']=[];config['roles']=[];config['state_dir']=str(out/'unused-state');save(out/'app.json',config)
        case={'backend':backend,'checks':{},'failures':[]}
        def cli(parts,cold=False):
            env=dict(os.environ)
            if cold:env.pop(cfg['provider']['key_env'],None)
            value=subprocess.run([str(exe),'--config',str(out/'app.json'),*map(str,parts)],env=env,capture_output=True,timeout=180)
            if value.returncode:raise RuntimeError('CLI '+str(parts[0])+' failed: '+value.stderr.decode(errors='replace')[:1200])
            return json.loads(value.stdout)
        try:
            cli(['database-migrate'],cold=True);before=requests()
            with host(exe,out/'app.json',out) as api:
                api('/sessions',{'id':'origin'});api('/sessions/origin/turns',{'turn_id':'one','prompt':'I prefer English replies.'})
                for _ in range(1200):
                    source=api('/sessions/origin')
                    if not source['job']['active']:break
                    time.sleep(0.05)
                else:raise RuntimeError('native source deadline reached')
                source_run=api('/runs/'+source['session']['turns'][0]['binding']['run_id'])['state'];save(out/'source.json',{'session':source,'run':source_run})
            request={'expected_revision':0,'session_id':'origin','turn_id':'one','request_id':'learn'};save(out/'request.json',request)
            first=cli(['memory-extract',out/'request.json']);save(out/'first-extraction.json',first)
            case['checks']['native_cli_extraction_completed']=first['proposals_recorded'] and first['run']['report']['stop_reason']=='completed' and bool(first['memory']['records'])
            request['expected_revision']=first['memory']['revision'];save(out/'repeat-request.json',request);known=requests()
            second=cli(['memory-extract',out/'repeat-request.json']);save(out/'immediate-reentry.json',second)
            case['checks']['immediate_cold_reentry_without_waiting_ten_second_ttl']=second==first
            case['checks']['no_native_dispatch_on_repeated_extraction']=requests()==known
            case['checks']['known_api_attempts_equal_two_unique_generations']=requests()-before==2
            case['known_native_usage']={key:source_run['report']['usage'][key]+first['run']['report']['usage'][key] for key in ['requests','input_tokens','output_tokens','cost_micros']}
            case['checks']['cold_inspection_retains_candidate']=cli(['memory'],cold=True)==first['memory']
        except Exception as error:case['failures'].append(str(error))
        case['failures'].extend('check failed: '+k for k,v in case['checks'].items() if not v);case['status']='failed' if case['failures'] else 'passed'
        save(out/'report.json',case);report['cases'].append(case);report['failures'].extend(backend+': '+v for v in case['failures'])
    report['completed']=True;report['status']='failed' if report['failures'] else 'passed';save(root/'report.json',report);print(json.dumps(report));raise SystemExit(1 if report['failures'] else 0)
if __name__=='__main__':main()
