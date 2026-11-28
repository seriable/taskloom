"""Actual SQLite/PostgreSQL CLI integration with an owned real Qdrant service.

Embedding vectors/usage are explicitly local HTTP protocol fixtures. A separate
owned loopback proxy forwards a real upsert then discards its completed ack to
exercise unknown publication without replay. No cloud/native model/SLO claim.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import threading
from urllib.parse import quote, urlsplit
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'database'))
from qdrant_fixture import owned, http as fixture_http, KEY_ENV, digest, save


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--taskloom',required=True,type=Path)
    parser.add_argument('--qdrant-fixture',required=True,type=Path)
    parser.add_argument('--postgres-fixture',required=True,type=Path)
    args=parser.parse_args();root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    qfixture=args.qdrant_fixture.resolve();owned(qfixture)
    target=json.loads((qfixture/'host-config.json').read_text());qkey=json.loads((qfixture/'credentials.json').read_text())[KEY_ENV]
    pgfixture=args.postgres_fixture.resolve();pgownership=json.loads((pgfixture/'ownership.json').read_text());cid=pgownership['container_id']
    if not re.fullmatch('[a-f0-9]{64}',cid):raise ValueError('invalid owned PostgreSQL identity')
    def docker(*parts):
        result=subprocess.run(['docker',*parts],capture_output=True,timeout=60)
        if result.returncode:raise RuntimeError('owned PostgreSQL operation failed')
        return result.stdout.decode().strip()
    pgstate=json.loads(docker('inspect',cid,'--format','{{json .}}'))
    if pgstate['Config']['Labels'].get('com.taskloom.database-acceptance')!=pgownership['nonce'] or pgstate['Image']!=pgownership['image_id']:
        raise ValueError('PostgreSQL ownership/image changed')
    pg=json.loads((pgfixture/'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL'];parsed=urlsplit(pg)
    if parsed.hostname!='127.0.0.1':raise ValueError('owned PostgreSQL must be loopback')
    exe=root/('taskloom'+args.taskloom.suffix)
    with args.taskloom.open('rb') as incoming,exe.open('xb') as outgoing:shutil.copyfileobj(incoming,outgoing)
    exe.chmod(args.taskloom.stat().st_mode)
    private=[pg,parsed.password,qkey];models={};mutex=threading.Lock();failures=[];proofs=[];cases=[]
    def clean(raw):
        text=raw.decode('utf-8',errors='replace') if isinstance(raw,bytes) else str(raw)
        for value in private:
            if value:text=text.replace(value,'[private fixture credential]')
        return text
    class Embedding(http.server.BaseHTTPRequestHandler):
        def log_message(self,*unused):pass
        def do_POST(self):
            try:
                size=int(self.headers['Content-Length']);assert 0<size<=256*1024
                body=json.loads(self.rfile.read(size));state=models[body['model']]
                namespace=json.dumps(['qdrant-cli',state['project']],separators=(',',':'))
                if state['backend']=='sqlite':
                    import sqlite3
                    with sqlite3.connect(state['database']) as connection:
                        row=connection.execute("SELECT body FROM taskloom_records WHERE namespace=? AND family='app_object' AND object_id='embedding-spend'",(namespace,)).fetchone()
                    ledger=json.loads(row[0])['value']
                else:
                    raw=docker('exec',cid,'psql','-X','-U','taskloom_accept','-d','taskloom_accept','-Atc',"SELECT body FROM taskloom_records WHERE namespace='"+namespace+"' AND family='app_object' AND object_id='embedding-spend'")
                    ledger=json.loads(raw)['value']
                with mutex:
                    assert ledger['requests']>state['calls'] and ledger['reserved_or_unknown_micros']>=10
                    state['calls']+=1;proofs.append({'backend':state['backend'],'project':state['project'],'durable_requests':ledger['requests'],'reserved_or_unknown_micros':ledger['reserved_or_unknown_micros']})
                response={'data':[{'index':i,'embedding':[1.0,0.5]} for i,_ in enumerate(body['input'])]}
                if not state['missing_usage']:response['usage']={'prompt_tokens':3,'total_tokens':3}
                raw=json.dumps(response).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
            except Exception as error:
                failures.append('embedding fixture failed: '+clean(error));self.send_error(500)
    provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Embedding);thread=threading.Thread(target=provider.serve_forever,daemon=True);thread.start()
    proxy_state={'requests':0,'upserts':0,'scope':None,'discarded_completed_ack':False}
    class LostAck(http.server.BaseHTTPRequestHandler):
        def log_message(self,*unused):pass
        def do_GET(self):self.forward()
        def do_PUT(self):self.forward()
        def forward(self):
            try:
                body=None if self.command=='GET' else json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                proxy_state['requests']+=1
                response=fixture_http(target['endpoint'],qkey,self.path,body,self.command)
                if self.command=='PUT':
                    proxy_state['upserts']+=1;assert response['result']['status']=='completed';proxy_state['discarded_completed_ack']=True
                    proxy_state['scope']=body['points'][0]['payload']['taskloom_scope']
                    self.send_error(502,'controlled proxy discarded completed upstream acknowledgement');return
                raw=json.dumps(response).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
            except Exception as error:
                failures.append('lost-ack fixture failed: '+clean(error));self.send_error(500)
    proxy=http.server.ThreadingHTTPServer(('127.0.0.1',0),LostAck);proxy_thread=threading.Thread(target=proxy.serve_forever,daemon=True);proxy_thread.start()
    report={'version':1,'completed':False,'checks':{},'failures':failures,'cases':cases,'scope':__doc__,'binary':digest(exe),'qdrant_provenance':json.loads((qfixture/'public.json').read_text()),'postgres_provenance':json.loads((pgfixture/'public.json').read_text()),'cloud_model_requests':0,'generation_requests':0}
    sqlite=root/'app.sqlite';sqlite_url='sqlite://'+quote(str(sqlite),safe='')+'?mode=rwc&cache=private'
    try:
        subprocess.run([str(exe),'init',str(root/'defaults.json')],capture_output=True,check=True,timeout=30)
        defaults=json.loads((root/'defaults.json').read_text())
        def setup(name,backend,url,endpoint=None):
            out=root/name;out.mkdir();project=uuid.uuid4().hex;model='qdrant-fixture-'+project
            config=dict(defaults,state_dir=str(out/'state'),provider={'type':'offline'},knowledge=True,database={'url_env':'TASKLOOM_DATABASE_URL','tenant':'qdrant-cli','project':project},retrieval=dict(defaults['retrieval'],embedding={'provider':{'type':'openai','base_url':f'http://127.0.0.1:{provider.server_port}/v1','key_env':'TASKLOOM_QDRANT_CLI_EMBED','namespace':'fixture-'+project},'model':model,'currency':'USD','input_micros_per_million_tokens':1000000,'max_request_cost_micros':10,'max_total_cost_micros':1000,'max_input_bytes':128*1024,'timeout_ms':5000},qdrant=dict(target,namespace='same-host-declared-namespace',endpoint=endpoint or target['endpoint'])))
            save(out/'lexical.json',config);hybrid=dict(config,retrieval=dict(config['retrieval'],mode='hybrid'));save(out/'hybrid.json',hybrid)
            state={'backend':backend,'project':project,'database':str(sqlite),'calls':0,'missing_usage':False};models[model]=state
            env=dict(os.environ,TASKLOOM_DATABASE_URL=url,TASKLOOM_QDRANT_CLI_EMBED='owned-local-embedding-fixture',**{KEY_ENV:qkey})
            def cli(parts,mode='lexical',cold=False,allow_failure=False):
                environment=dict(env)
                if cold:environment.pop(KEY_ENV,None);environment.pop('TASKLOOM_QDRANT_CLI_EMBED',None)
                result=subprocess.run([str(exe),'--config',str(out/(mode+'.json')),*map(str,parts)],env=environment,capture_output=True,timeout=90)
                label=str(len(list(out.glob('cli-*.log')))).zfill(3)
                with (out/('cli-'+label+'.log')).open('x',encoding='utf-8') as stream:stream.write(clean(result.stdout)+'\n'+clean(result.stderr))
                if result.returncode and not allow_failure:raise RuntimeError(name+' CLI '+str(parts[0])+' failed; sanitized log retained')
                if allow_failure:return result
                try:return json.loads(result.stdout)
                except json.JSONDecodeError:return clean(result.stdout)
            cli(['database-migrate'])
            for identity,text in [('alpha','Source alpha retains exact local provenance for the Qdrant app.'),('beta','Source beta is a separately imported local document.')]:
                path=out/(identity+'.txt');path.write_text(text,encoding='utf-8');cli(['import',identity,identity,path])
            return out,config,state,cli
        def triple(status):return [status['corpus_revision'],status['configuration_binding'],status['index_binding']]
        for backend,url in [('sqlite',sqlite_url),('postgres',pg)]:
            out,config,state,cli=setup(backend,backend,url);case={'backend':backend,'checks':{}};cases.append(case)
            initial=cli(['index-status'],cold=True);lexical=cli(['search','source'],cold=True)
            case['checks']['cold_lexical_reads_without_embedding_or_qdrant_credentials']=state['calls']==0 and bool(lexical['hits']) and initial['qdrant']['ready'] is False
            cli(['index-build',initial['corpus_revision'],initial['configuration_binding']]);built=cli(['index-status'],cold=True)
            before=state['calls'];refused=cli(['search','source'],mode='hybrid',allow_failure=True)
            case['checks']['hybrid_refuses_unpublished_index_before_query_embedding']=refused.returncode!=0 and state['calls']==before
            published=cli(['index-publish-qdrant',*triple(built)]);save(out/'published.json',published)
            cold=cli(['index-status'],mode='hybrid',cold=True);case['checks']['complete_receipt_cold_load_has_no_model_request']=cold['qdrant']['ready'] is True and state['calls']==before
            found=cli(['search','source'],mode='hybrid');save(out/'search.json',found)
            case['checks']['new_cli_process_uses_real_qdrant_and_own_local_sources']=len(found['hits'])==2 and all(str(out).casefold() in hit['chunk']['source'].replace('\\?\\','').casefold() for hit in found['hits'])
            spending=cli(['index-status'],cold=True)['embedding_spend'];case['checks']['build_and_query_use_durable_embedding_ledger']=spending['requests']==2 and spending['settled_micros']==6 and spending['reserved_or_unknown_micros']==0
            state['missing_usage']=True;cli(['search','source'],mode='hybrid');unknown=cli(['index-status'],cold=True)['embedding_spend'];case['checks']['query_missing_usage_reservation_is_retained']=unknown['reserved_or_unknown_micros']==10 and unknown['settled_micros']==6;state['missing_usage']=False
            changed=out/'changed.txt';changed.write_text('A new corpus revision cannot query the previous frozen index.',encoding='utf-8');cli(['import','changed','changed',changed]);before=state['calls'];stale=cli(['search','source'],mode='hybrid',allow_failure=True)
            case['checks']['stale_full_snapshot_is_refused_before_paid_query']=stale.returncode!=0 and state['calls']==before and cli(['index-status'],mode='hybrid',cold=True)['qdrant']['ready'] is False
            case['fixture_embedding_http_requests']=state['calls']
        # A real upstream Qdrant write succeeds. Only the owned loopback proxy
        # discards its acknowledgement; this is not a multihost partition test.
        out,config,state,cli=setup('lost-ack','sqlite',sqlite_url,f'http://127.0.0.1:{proxy.server_port}/');status=cli(['index-status']);cli(['index-build',status['corpus_revision'],status['configuration_binding']]);status=cli(['index-status'])
        failed=cli(['index-publish-qdrant',*triple(status)],allow_failure=True);before=proxy_state['requests'];again=cli(['index-publish-qdrant',*triple(status)],allow_failure=True);cold_again=cli(['index-publish-qdrant',*triple(status)],cold=True,allow_failure=True);blocked=cli(['search','source'],mode='hybrid',allow_failure=True);retained=cli(['index-status'],mode='hybrid',cold=True)
        report['checks']['real_upsert_lost_ack_retains_unknown_without_retry']=failed.returncode!=0 and again.returncode!=0 and cold_again.returncode!=0 and proxy_state['discarded_completed_ack'] and proxy_state['upserts']==1 and proxy_state['requests']==before
        report['checks']['unknown_publication_never_authorizes_query_or_paid_embedding']=blocked.returncode!=0 and state['calls']==1 and retained['qdrant']['ready'] is False and retained['qdrant']['journal']['batches'][0]['state']=='pending'
        owned(qfixture)
        collection=target['collection'];total=fixture_http(target['endpoint'],qkey,'/collections/'+collection+'/points/count',{'exact':True})['result']['count'];points=fixture_http(target['endpoint'],qkey,'/collections/'+collection+'/points/scroll',{'limit':100,'with_payload':True,'with_vector':False})['result']['points']
        counts={}
        for point in points:
            scope=point['payload']['taskloom_scope'];counts[scope]=counts.get(scope,0)+1
        report['checks']['real_service_has_two_isolated_complete_scopes_and_one_unknown_point']=total==5 and sorted(counts.values())==[1,2,2] and counts.get(proxy_state['scope'])==1
        report['real_point_counts']=counts;report['lost_ack_proxy']=proxy_state
        report['checks']['all_http_fixture_embeddings_observe_durable_reservations']=len(proofs)==7 and all(proof['reserved_or_unknown_micros']>=10 for proof in proofs)
        report['completed']=True
    except Exception as error:failures.append(clean(error))
    finally:
        provider.shutdown();provider.server_close();proxy.shutdown();proxy.server_close();thread.join(timeout=5);proxy_thread.join(timeout=5)
        save(root/'embedding-reservation-proofs.json',proofs)
        for case in cases:failures.extend(case['backend']+' check failed: '+name for name,passed in case['checks'].items() if not passed)
        failures.extend('check failed: '+name for name,passed in report['checks'].items() if not passed)
        report['status']='passed' if report['completed'] and not failures else 'failed';source=Path(__file__).resolve().parents[2]
        report['sources']={name:digest(source/name) for name in ['Cargo.toml','Cargo.lock','src/runtime/rag/qdrant.rs','src/runtime/app/retrieval.rs','src/runtime/app/cli.rs','examples/operations/qdrant_app.py']}
        report['logs']={path.relative_to(root).as_posix():digest(path) for path in root.rglob('cli-*.log')};save(root/'report.json',report)
    print(json.dumps(report));raise SystemExit(0 if report['status']=='passed' else 1)


if __name__=='__main__':main()
