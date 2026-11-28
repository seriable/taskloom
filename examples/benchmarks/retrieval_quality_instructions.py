"""Bounded local query-instruction diagnostic against captured full-document indexes.
All queries and labels are unchanged observed regression data. No blind quality claim.
"""
import argparse, hashlib, json, math, os, time, urllib.request
from pathlib import Path

def read(p): return json.loads(p.read_text(encoding='utf-8'))
def save(p,v): p.write_text(json.dumps(v,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
def cosine(a,b): return sum(x*y for x,y in zip(a,b))/math.sqrt(sum(x*x for x in a)*sum(x*x for x in b))
def main():
 p=argparse.ArgumentParser(description=__doc__); p.add_argument('out',type=Path); a=p.parse_args(); a.out.mkdir(exist_ok=False)
 url='http://127.0.0.1:18087'; health=read_response(url+'/health'); dataset=read(Path('tests/data/retrieval-repository-v2.json'))
 indexes={name:read(Path(root)/'state/retrieval/index.json') for name,root in [('body','target/retrieval-repository-instructed-20261128-1'),('title_body','target/retrieval-document-context-20261128-1')]}
 corpus=read(Path('target/retrieval-document-context-20261128-1/state/knowledge/corpus.json')); docs={c['id']:c['document_id'] for c in corpus['chunks']}
 prefixes={
 'question':'Instruct: Given a question, retrieve relevant passages that answer the question\nQuery: ',
 'technical':'Instruct: Given a question about software, retrieve relevant passages from technical documentation that answer the question\nQuery: '}
 report={'scope':__doc__,'start_health':health,'max_requests':6,'attempts':[],'modes':{}}; save(a.out/'report.json',report)
 for name,prefix in prefixes.items():
  vectors=[]
  for offset in range(0,len(dataset['queries']),32):
   row={'ordinal':len(report['attempts'])+1,'mode':name,'offset':offset,'status':'reserved_before_dispatch'}; report['attempts'].append(row); save(a.out/'report.json',report)
   payload={'model':'Qwen/Qwen3-Embedding-0.6B','input':[prefix+q['text'] for q in dataset['queries'][offset:offset+32]]}
   request=urllib.request.Request(url+'/v1/embeddings',data=json.dumps(payload).encode(),headers={'Authorization':'Bearer '+os.environ['TASKLOOM_LOCAL_API_KEY'],'Content-Type':'application/json'})
   with urllib.request.urlopen(request,timeout=90) as response: body=json.loads(response.read(8*1024*1024))
   ordered=sorted(body['data'],key=lambda r:r['index']); assert [r['index'] for r in ordered]==list(range(len(payload['input'])))
   vectors.extend(r['embedding'] for r in ordered); row['status']='received'; save(a.out/f'response-{row["ordinal"]}.json',body); save(a.out/'report.json',report)
  result={}
  for kind,index in indexes.items():
   cases=[]
   for q,v in zip(dataset['queries'],vectors):
    ranking=sorted([(k,cosine(v,w)) for k,w in index['vectors'].items()],key=lambda x:(-x[1],x[0])); admitted=[k for k,s in ranking if s>=.43][:20]
    cases.append({'id':q['id'],'split':q['split'],'relevant':q['relevant'],'maximum_score':ranking[0][1],'best_relevant_score':next((s for k,s in ranking if docs[k] in q['relevant']),None),'top20':ranking[:20],'admission':any(docs[k] in q['relevant'] for k in admitted)})
   result[kind]={'cases':cases,'summary':{split:{'positive_admitted':sum(c['admission'] for c in cases if c['split']==split and c['relevant']),'negative_rejected_without_identifiers':sum(c['maximum_score']<.43 for c in cases if c['split']==split and not c['relevant'])} for split in ['calibration','fixed_evaluation']}}
  report['modes'][name]={'prefix':prefix,'results':result}; report['end_health']=read_response(url+'/health'); save(a.out/'report.json',report)
  print(name,{k:v['summary'] for k,v in result.items()},flush=True)

def read_response(url):
 with urllib.request.urlopen(url,timeout=10) as response:return json.loads(response.read())
if __name__=='__main__': main()
