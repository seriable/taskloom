/* Project-scoped UI. All remote data is text, and credentials stay in memory. */
(() => {
  'use strict';
  const $=id=>document.getElementById(id), json=TaskloomJson;
  let token='', base='', user='', generation=0, cookieSession=false, csrf='';
  let followGeneration=0, followTimer, followDeadlineTimer, followAbort;
  function stopEvents(){followGeneration++;clearTimeout(followTimer);clearTimeout(followDeadlineTimer);followAbort?.abort();followAbort=undefined;}
  const show=(id,value)=>{$(id).textContent=typeof value==='string'?value:json.stringify(value,true);};
  const identifier=value=>{if(!/^[A-Za-z0-9_-]{1,128}$/.test(value))throw Error('ID 只能包含字母、数字、连字符或下划线');return encodeURIComponent(value);};
  const scope=value=>{if(!/^[A-Za-z0-9_.-]{1,64}$/.test(value)||value==='.'||value==='..')throw Error('无效的租户或项目');return encodeURIComponent(value);};
  function clear(){
    stopEvents();generation++;token='';base='';user='';cookieSession=false;csrf='';$('token').value='';$('semantic-memory').hidden=true;
    for(const id of ['identity','message','queue-state','shared-state','memory-state','operations-state','private-state','events-status','events-state'])show(id,'');
    // Switching principals must discard drafts as well as fetched records.
    for(const id of ['prompt','queue-id','record-id','ticket','note','private-prompt','memory-query','memory-revision','memory-id','memory-note','memory-value','memory-key','extract-revision'])$(id).value='';
    $('private-use-memory').checked=false;
    const stamp=Date.now();$('run-id').value='run-'+stamp;$('private-session').value='session-'+stamp;$('private-turn').value='turn-1';$('extract-request').value='extract-'+stamp;
  }
  async function api(path,body,signal){
    if(!token&&!cookieSession)throw Error('请先连接项目');const epoch=generation;
    const abort=new AbortController(),cancel=()=>abort.abort();
    if(signal?.aborted)throw Error('观察已停止');signal?.addEventListener('abort',cancel,{once:true});
    const timer=setTimeout(cancel,30000);
    try{
      const response=await fetch(base+path,{method:body===undefined?'GET':'POST',redirect:'error',headers:{...(token?{Authorization:'Bearer '+token}:{}),...(body===undefined?{}:{'Content-Type':'application/json',...(cookieSession?{'x-taskloom-csrf':csrf}:{})})},body:body===undefined?undefined:json.stringify(body),cache:'no-store',signal:abort.signal});
      const reader=response.body?.getReader();if(!reader)throw Error('响应没有 JSON 内容');
      const chunks=[];let size=0;
      for(;;){const {value,done}=await reader.read();if(done)break;size+=value.byteLength;if(size>4*1024*1024){await reader.cancel();throw Error('响应过大，操作结果可能已保存，请检查原 ID');}chunks.push(value);}
      if(epoch!==generation)throw Error('连接已更改，旧响应已丢弃');
      const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.byteLength;}
      const value=json.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));if(!response.ok)throw Error(value.error||'HTTP '+response.status);return value;
    }catch(error){if(abort.signal.aborted)throw Error('请求已取消或超时；操作结果可能已保存，请检查原 ID');throw error;}
    finally{clearTimeout(timer);signal?.removeEventListener('abort',cancel);}
  }
  const action=work=>async()=>{try{$('message').className='muted';await work();}catch(error){show('message',error.message);$('message').className='error';}};
  $('connect').onclick=action(async()=>{const entered=$('token').value;clear();base='/tenants/'+scope($('tenant').value)+'/projects/'+scope($('project').value)+'/api';token=entered;const epoch=generation;try{if(!entered){const login=await fetch('/auth/session',{cache:'no-store',redirect:'error'});if(epoch!==generation)throw Error('连接已更改');if(!login.ok)throw Error('请提供用户 Bearer，或先完成宿主配置的 SSO 登录');const session=await login.json();if(epoch!==generation)throw Error('连接已更改');cookieSession=session.authenticated===true;csrf=session.csrf;}$('sso-logout').hidden=!cookieSession;const value=await api('/identity');user=value.user;$('semantic-memory').hidden=value.semantic_memory_enabled!==true;show('identity',value);show('message','已连接。所有授权仍由服务端逐次检查。');}catch(error){if(epoch===generation)clear();throw error;}});
  $('disconnect').onclick=clear;
  $('sso-logout').onclick=action(async()=>{const epoch=generation;const info=await fetch('/auth/session',{cache:'no-store',redirect:'error'});if(epoch!==generation)return;if(!info.ok)throw Error('没有有效的网页登录');const session=await info.json();if(epoch!==generation)return;const response=await fetch('/auth/logout',{method:'POST',redirect:'error',headers:{'x-taskloom-csrf':session.csrf}});if(epoch!==generation)return;if(!response.ok)throw Error('退出失败，请检查当前登录状态');clear();$('sso-logout').hidden=true;show('message','网页登录已撤销。');});
  fetch('/auth/session',{cache:'no-store',redirect:'error'}).then(response=>{$('sso-login').hidden=response.status===404;$('sso-logout').hidden=response.status!==200;}).catch(()=>{});
  $('run-id').value='run-'+Date.now();
  $('submit').onclick=action(async()=>{const result=await api('/queue/jobs',{task:{type:'run',id:identifier($('run-id').value),prompt:$('prompt').value},actor:user});show('queue-state',result);show('message','任务已持久化；提交成功不等于模型已完成。');});
  $('queue-refresh').onclick=action(async()=>show('queue-state',await api('/queue/jobs')));
  $('queue-inspect').onclick=action(async()=>show('queue-state',await api('/queue/jobs/'+identifier($('queue-id').value))));
  $('shared-refresh').onclick=action(async()=>show('shared-state',await api('/'+$('kind').value)));
  $('inspect').onclick=action(async()=>show('shared-state',await api('/'+$('kind').value+'/'+identifier($('record-id').value))));
  $('diagnostics').onclick=action(async()=>{const kind=({runs:'run',workflows:'workflow',sessions:'session'})[$('kind').value];show('shared-state',await api('/diagnostics/'+kind+'/'+identifier($('record-id').value)));});
  for(const [id,approved] of [['approve',true],['reject',false]])$(id).onclick=action(async()=>{if($('kind').value!=='runs')throw Error('此按钮只处理运行审批；父工作流/会话请使用对应 API');show('shared-state',await api('/runs/'+identifier($('record-id').value)+'/approvals/'+identifier($('ticket').value),{approved,actor:user,note:$('note').value}));show('message','决定已保存；执行需按当前队列/运行状态显式恢复。');});
  const revision=()=>{const raw=$('memory-revision').value;if(!/^(0|[1-9][0-9]*)$/.test(raw))throw Error('请输入保存的准确版本');return json.parse(raw);};
  $('memory-read').onclick=action(async()=>show('memory-state',await api('/memory')));
  $('memory-recall').onclick=action(async()=>show('memory-state',await api('/memory/recall',{text:$('memory-query').value})));
  $('memory-semantic').onclick=action(async()=>show('memory-state',await api('/memory/semantic-recall',{text:$('memory-query').value})));
  for(const [id,accept] of [['memory-accept',true],['memory-reject',false]])$(id).onclick=action(async()=>show('memory-state',await api('/memory/review',{expected_revision:revision(),id:$('memory-id').value,accept,replace:false,note:$('memory-note').value})));
  $('memory-edit').onclick=action(async()=>show('memory-state',await api('/memory/edit',{expected_revision:revision(),id:$('memory-id').value,value:$('memory-value').value,note:$('memory-note').value})));
  $('memory-forget').onclick=action(async()=>show('memory-state',await api('/memory/forget',{expected_revision:revision(),kind:$('memory-kind').value,key:$('memory-key').value})));
  const privatePath=()=>'/private/sessions/'+identifier($('private-session').value);
  const privateTurn=()=>identifier($('private-turn').value);
  $('private-session').value='session-'+Date.now();$('private-turn').value='turn-1';$('extract-request').value='extract-'+Date.now();
  $('private-create').onclick=action(async()=>show('private-state',await api('/private/sessions',{id:identifier($('private-session').value)})));
  $('private-send').onclick=action(async()=>show('private-state',await api(privatePath()+($('private-use-memory').checked?'/memory-turns':'/turns'),{turn:privateTurn(),prompt:$('private-prompt').value})));
  $('private-status').onclick=action(async()=>{const history=await api(privatePath());const status=await api(privatePath()+'/status');show('private-state',{history,status});});
  $('private-resume').onclick=action(async()=>show('private-state',await api(privatePath()+'/resume',{turn:privateTurn()})));
  $('private-cancel').onclick=action(async()=>show('private-state',await api(privatePath()+'/cancel',{})));
  $('private-extract').onclick=action(async()=>{const raw=$('extract-revision').value;if(!/^(0|[1-9][0-9]*)$/.test(raw))throw Error('请输入准确记忆版本');show('private-state',await api('/private/memory/extract',{expected_revision:json.parse(raw),session_id:identifier($('private-session').value),turn_id:privateTurn(),request_id:identifier($('extract-request').value)}));});
  $('operations').onclick=action(async()=>show('operations-state',await api('/operations')));
  $('stop-events').onclick=()=>{stopEvents();show('events-status','已停止观察。保存的运行记录可继续按游标检查。');};
  $('follow-events').onclick=action(async()=>{
    stopEvents();const kind=$('kind').value,id=identifier($('record-id').value);
    if(!['runs','workflows'].includes(kind))throw Error('事件观察仅支持共享运行或工作流');
    const epoch=followGeneration,connection=generation,deadline=Date.now()+30000;
    followDeadlineTimer=setTimeout(()=>{if(epoch===followGeneration){stopEvents();show('events-status','30 秒观察已结束。');}},30000);
    let after=0n,events=[],eventSizes=[],eventBytes=0,omitted=0;
    show('events-state','');show('events-status','正在观察 '+kind+'/'+id);
    const tick=async()=>{
      if(epoch!==followGeneration||connection!==generation)return;
      followAbort=new AbortController();
      try{
        const page=await api('/'+kind+'/'+id+'/events?after='+after,undefined,followAbort.signal);
        if(epoch!==followGeneration||connection!==generation)return;
        if(!Array.isArray(page.events)||page.events.length>200||!(page.next instanceof json.ExactNumber)||!/^(0|[1-9][0-9]{0,19})$/.test(page.next.source))throw Error('无效的事件页');
        const next=BigInt(page.next.source);if(next>18446744073709551615n||next!==after+BigInt(page.events.length))throw Error('事件游标与返回数量不一致');
        after=next;
        for(const event of page.events){const size=new TextEncoder().encode(json.stringify(event)).length;events.push(event);eventSizes.push(size);eventBytes+=size;while(events.length>500||eventBytes>1024*1024){events.shift();eventBytes-=eventSizes.shift();omitted++;}}
        show('events-state',{kind,id,cursor:new json.ExactNumber(after.toString()),omitted_from_this_view:omitted,events});
        const active=page.job?.active===true,more=page.events.length===200;
        if(Date.now()>=deadline||!active&&!more){stopEvents();show('events-status','观察结束；保存的记录不受影响。');return;}
        followTimer=setTimeout(tick,1000);
      }catch(error){if(epoch===followGeneration&&connection===generation){stopEvents();show('events-status','观察已停止：'+error.message);}}
    };
    await tick();
  });
  window.addEventListener('pagehide',stopEvents);
})();
