import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { TaskloomClient, TaskloomError } from './index.js';

async function server(status=200, body='{"ok":true}', location) {
  const requests=[];
  const instance=http.createServer(async(req,res)=>{let raw='';for await(const chunk of req)raw+=chunk;requests.push({method:req.method,path:req.url,body:raw?JSON.parse(raw):null});res.writeHead(status,{'Content-Type':'application/json',...(location?{Location:location}:{})});res.end(body);});
  await new Promise(resolve=>instance.listen(0,'127.0.0.1',resolve));
  return {requests,url:`http://127.0.0.1:${instance.address().port}`,close:()=>new Promise(resolve=>instance.close(resolve))};
}
test('real HTTP exact approval, turn and event cursor',async()=>{
  const host=await server();try{
    const client=new TaskloomClient(host.url+'/tenants/t/projects/p/api','test-secret');
    await client.run('one','hello');await client.approve('one','ticket','alice','checked');await client.sessionTurn('session','turn-1','next');await client.events('one',undefined,200);await client.inspect('session-'+'a'.repeat(64));
    assert.deepEqual(host.requests[0].body,{id:'one',prompt:'hello'});
    assert.equal(host.requests[1].path,'/tenants/t/projects/p/api/runs/one/approvals/ticket');assert.deepEqual(host.requests[1].body,{approved:true,actor:'alice',note:'checked'});
    assert.deepEqual(host.requests[2].body,{turn_id:'turn-1',prompt:'next'});assert.ok(host.requests[3].path.endsWith('/events?after=200'));
    assert.ok(host.requests[4].path.endsWith('/runs/session-'+'a'.repeat(64)));
    assert.equal(JSON.stringify(client),'{}');
  }finally{await host.close();}
});
test('redirect never forwards credentials',async()=>{
  const destination=await server();const origin=await server(302,'',destination.url+'/capture');try{
    await assert.rejects(new TaskloomClient(origin.url+'/api','test-secret').resume('one'),error=>error instanceof TaskloomError);
    assert.equal(origin.requests.length,1);assert.equal(destination.requests.length,0);
  }finally{await origin.close();await destination.close();}
});
test('bounds, typed conflicts and invalid response',async()=>{
  for(const [status,body,kind] of [[409,'private-data','conflict'],[200,'x'.repeat(2048),'response_limit_outcome_unknown'],[200,'bad','invalid_response_outcome_unknown']]){
    const host=await server(status,body);try{
      await assert.rejects(new TaskloomClient(host.url+'/api','test',{maxResponseBytes:1024}).inspect('one'),error=>error.kind===kind&&!error.message.includes('private-data'));
      assert.equal(host.requests.length,1);
    }finally{await host.close();}
  }
});
test('pre-cancel and unsafe paths never dispatch',async()=>{
  for(const url of ['http://example.com/api','https://a:b@example.com/api','https://example.com/api?key=x'])assert.throws(()=>new TaskloomClient(url,'test'));
  const host=await server();try{
    const client=new TaskloomClient(host.url+'/api','test');const controller=new AbortController();controller.abort();
    await assert.rejects(client.resume('one',{signal:controller.signal}),error=>error.kind==='cancelled_before_dispatch');
    for(const path of ['/../capture','//capture','/runs?key=x'])await assert.rejects(client.request('GET',path));
    for(const body of [{budget:NaN},{revision:9007199254740992},{value:Infinity}])await assert.rejects(client.request('POST','/runs',body));
    assert.equal(host.requests.length,0);
  }finally{await host.close();}
});
test('server u64 revisions fail instead of silently rounding',async()=>{
  const host=await server(200,'{"revision":9007199254740993,"text":"9007199254740993"}');try {
    await assert.rejects(new TaskloomClient(host.url+'/api','test').inspect('one'),error=>error.kind==='invalid_response_outcome_unknown');
    assert.equal(host.requests.length,1);
  }finally{await host.close();}
});
