"""Actual local embedding score diagnostic, separate from application acceptance.

Reuses a captured index, never invents vectors or rebuilds it. Raw/publisher-style
query instructions are distinct experiments, not hidden prompt rewrites. Source
labels are visible development labels; thresholds selected here need a separate
actual application run and fresh evaluation before a quality claim.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import time
import urllib.request
from urllib.parse import urlsplit

from native_retrieval import save


def cosine(left,right):
    if len(left)!=len(right) or not left:raise ValueError('embedding dimensions changed')
    if any(not math.isfinite(x) for x in left+right):raise ValueError('nonfinite embedding')
    denominator=math.sqrt(sum(x*x for x in left)*sum(x*x for x in right))
    if not denominator:raise ValueError('zero embedding')
    return sum(x*y for x,y in zip(left,right))/denominator


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out',type=Path)
    parser.add_argument('--campaign',type=Path,required=True)
    parser.add_argument('--embedding-url',required=True)
    parser.add_argument('--key-env',default='TASKLOOM_LOCAL_API_KEY')
    parser.add_argument('--mode',choices=['raw','instruct','both'],default='both')
    args=parser.parse_args()
    url=urlsplit(args.embedding_url)
    if url.scheme!='http' or url.hostname not in ['127.0.0.1','localhost','::1'] or url.username or url.query:
        raise ValueError('explicit zero-fee loopback service required')
    root=args.out.resolve();root.mkdir(parents=True,exist_ok=False)
    campaign=args.campaign.resolve()
    dataset=json.loads((campaign/'dataset.json').read_text(encoding='utf-8'))
    index=json.loads((campaign/'state/retrieval/index.json').read_text(encoding='utf-8'))
    corpus=json.loads((campaign/'state/knowledge/corpus.json').read_text(encoding='utf-8'))
    documents={c['id']:c['document_id'] for c in corpus['chunks']}
    if set(index['vectors'])!=set(documents):raise ValueError('captured corpus/index source mismatch')
    queries=dataset['queries']
    if not 1<=len(queries)<=128:raise ValueError('bounded diagnostic query set required')
    report={'scope':__doc__,'source_index_sha256':hashlib.sha256((campaign/'state/retrieval/index.json').read_bytes()).hexdigest(),
            'credential_environment':args.key_env,'api_fee_currency':'USD','api_input_price':0,
            'max_requests':16,'attempts':[],'modes':{},'identity_qualification':'Use the same already verified owned local service as the captured index; no independent server attestation or model fallback.'}
    key=os.environ[args.key_env]
    instructions={'raw':'','instruct':'Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery: '}
    if args.mode!='both':instructions={args.mode:instructions[args.mode]}
    for mode,prefix in instructions.items():
        vectors=[]
        for offset in range(0,len(queries),16):
            if len(report['attempts'])>=report['max_requests']:raise ValueError('diagnostic request allocation exhausted')
            entry={'ordinal':len(report['attempts'])+1,'mode':mode,'start':offset,'status':'reserved_before_dispatch'}
            report['attempts'].append(entry);save(root/'report.json',report)
            started=time.monotonic()
            payload={'model':'Qwen/Qwen3-Embedding-0.6B','input':[prefix+q['text'] for q in queries[offset:offset+16]]}
            request=urllib.request.Request(args.embedding_url.rstrip('/')+'/embeddings',data=json.dumps(payload).encode(),
                headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})
            try:
                with urllib.request.urlopen(request,timeout=60) as response:body=json.loads(response.read(8*1024*1024))
                ordered=sorted(body['data'],key=lambda row:row['index'])
                if body.get('model')!=payload['model'] or [v['index'] for v in ordered]!=list(range(len(payload['input']))):raise ValueError('embedding model/count/index mismatch')
                vectors.extend(row['embedding'] for row in ordered)
                entry.update(status='response_received',usage=body.get('usage'))
                save(root/f'response-{entry["ordinal"]}.json',body)
            finally:
                entry['wall_ms']=round((time.monotonic()-started)*1000);save(root/'report.json',report)
        cases=[]
        for query,vector in zip(queries,vectors):
            ranked=sorted(((identity,cosine(vector,v)) for identity,v in index['vectors'].items()),key=lambda pair:(-pair[1],pair[0]))
            relevant=set(query['relevant'])
            best=next((score for identity,score in ranked if documents[identity] in relevant),None)
            cases.append({'id':query['id'],'relevant':query['relevant'],'best_relevant_score':best,
                          'maximum_score':ranked[0][1],'top20':ranked[:20]})
        maximum_negative=max(c['maximum_score'] for c in cases if not c['relevant'])
        thresholds=[round(t/100,2) for t in range(35,66)]
        report['modes'][mode]={'query_prefix':prefix,'cases':cases,'maximum_negative_score':maximum_negative,
             'threshold_bounds':[{'cutoff':threshold,'possible_positive_admission':sum(c['best_relevant_score']>=threshold for c in cases if c['relevant'])/sum(bool(c['relevant']) for c in cases),
                                  'empty_negatives':sum(c['maximum_score']<threshold for c in cases if not c['relevant'])} for threshold in thresholds]}
        save(root/'report.json',report)
        feasible=[v for v in report['modes'][mode]['threshold_bounds'] if v['possible_positive_admission']>=.9 and v['empty_negatives']==sum(not c['relevant'] for c in cases)]
        print(mode,'max_negative',maximum_negative,'feasible admission bounds',feasible,flush=True)


if __name__=='__main__':main()
