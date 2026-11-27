// This extends the run console with persisted parent controllers. Model/source
// text is always assigned through textContent, never interpolated into HTML.
let parentSelection=null,childOwner=null,parentSignature='';
const panel=document.createElement('section');
panel.innerHTML=`<h2>Workflows & sessions</h2>
<div class="row"><div><label for="parent-kind">Execution type</label><select id="parent-kind"><option value="workflow">Role DAG</option><option value="peer">PEER plan / execute / review</option><option value="session">Conversation session</option></select></div><div><label for="parent-id">Workflow or session ID</label><input id="parent-id"></div></div>
<label for="parent-prompt">Task or message</label><textarea id="parent-prompt"></textarea>
<div id="dag-form"><label for="dag-definition">DAG definition (registered role names only)</label><textarea id="dag-definition">{"nodes":[{"id":"execute","task":"executor","depends_on":[],"condition":{"type":"always"},"allow_skipped_dependencies":false,"select":{},"params":null}]}</textarea></div>
<p class="muted">Roles are configured by the host. PEER uses planner, executor, writer and reviewer, at most two tasks and one revision. Offline mode provides source excerpts only.</p>
<button id="parent-create">Create</button><button id="session-send" class="secondary">Send session turn</button><details><summary>Available role contracts</summary><pre id="role-contracts"></pre></details>
<div class="grid"><div><h2>Saved workflows & sessions</h2><div id="parents"></div></div><div><h2 id="parent-title">Select a workflow or session</h2><div id="parent-budget"></div><p id="parent-error" class="error"></p><div id="parent-controls" class="hidden"><button id="parent-resume">Resume parent</button><button id="parent-pause" class="secondary">Pause parent</button><button id="parent-cancel" class="danger">Cancel parent</button><button id="session-abandon" class="danger">Acknowledge failed turn</button></div><div id="parent-nodes"></div><details><summary>Saved handoffs, review feedback & history</summary><pre id="parent-state"></pre></details></div></div>`;
document.querySelector('main>.grid').before(panel);
$('parent-id').value='workflow-'+Date.now();
const parentPath=owner=>'/'+(owner.kind==='workflow'?'workflows':'sessions')+'/'+encodeURIComponent(owner.id);
const nodePath=owner=>parentPath(owner)+(owner.kind==='workflow'?'/nodes/'+encodeURIComponent(owner.node):'');
const rawApi=api;
api=async function(path,method='GET',body){
  if(method!=='GET'&&childOwner?.run===selected&&path.startsWith('/runs/'+encodeURIComponent(selected)+'/')){
    if(!childOwner.editable)throw Error('This is an archived turn. Select the active parent to make decisions.');
    const suffix=path.slice(('/runs/'+encodeURIComponent(selected)).length);
    path=(/^\/(resume|pause|cancel)$/.test(suffix)?parentPath(childOwner):nodePath(childOwner))+suffix;
  }
  return rawApi(path,method,body);
};
const rawDetail=detailRun;
detailRun=async function(){
  if(childOwner?.run!==selected)childOwner=null;
  await rawDetail();
  if(selected.length>64){
    show('selected',selected+' · '+(childOwner?childOwner.kind+' '+childOwner.id:'managed child (inspect through its parent)'));
    const editable=childOwner?.editable;
    $('controls').classList.toggle('hidden',!editable);
    $('approval').classList.toggle('hidden',!editable||!current.tool_steps.some(s=>s.progress.type==='awaiting_approval'));
    if(editable){
      const data=await rawApi(parentPath(childOwner));
      const terminal=!!data.state&&['completed','failed','cancelled','deadline','dispatch_limit','budget_limit'].includes(data.state.status);
      if(data.session&&data.session.active?.run_id!==selected){
        childOwner.editable=false;$('controls').classList.add('hidden');$('approval').classList.add('hidden');
      }
      $('resume').disabled=data.job.active||terminal;
      $('pause').disabled=!data.job.active;
      $('cancel').disabled=terminal;
      show('job',data.job.error||'Child execution is controlled by its parent.');
    }
  }
  renderRecovery();
};
function renderRecovery(){
  const region=$('recovery');
  const unknown=current.tool_steps.filter(s=>['uncertain','in_flight'].includes(s.progress.type));
  const signature=JSON.stringify([selected,childOwner?.editable,unknown]);
  if(region.dataset.signature===signature)return;
  region.dataset.signature=signature;region.replaceChildren();
  region.classList.toggle('hidden',!unknown.length);
  if(!unknown.length)return;
  const title=document.createElement('h2');title.textContent='Effect needs reconciliation';region.append(title);
  const text=document.createElement('p');text.textContent='Inspect the external outcome first. Recording a result saves an observation; Resume is a separate action. Uncertain writes cannot be blindly retried.';region.append(text);
  for(const step of unknown){
    const saved=document.createElement('pre');saved.textContent=JSON.stringify({call:step.call,effect:step.definition.effect,progress:step.progress},null,2);region.append(saved);
    if(selected.length>64&&!childOwner?.editable)continue;
    const form=document.createElement('div');
    const operator=document.createElement('input');operator.placeholder='Operator';operator.value='local-operator';operator.setAttribute('aria-label','Recovery operator');
    const note=document.createElement('input');note.placeholder='Evidence supporting this reconciliation';note.setAttribute('aria-label','Recovery evidence');
    const output=document.createElement('textarea');output.placeholder='Observed tool result as JSON';output.setAttribute('aria-label','Observed tool result');
    const error=document.createElement('label');const checkbox=document.createElement('input');checkbox.type='checkbox';checkbox.style.width='auto';error.append(checkbox,' Observed result is an error');
    form.append(operator,note,output,error);
    for(const [label,type]of [['Record observed result','record_result'],['Abort call','abort'],...(step.definition.effect==='non_idempotent'?[]:[['Retry safe operation','retry_safe']])]){
      const button=document.createElement('button');button.textContent=label;button.onclick=()=>act(async()=>{
        const decision={type,actor:operator.value,note:note.value};
        if(type==='record_result'){decision.output=TaskloomJson.parse(output.value);decision.is_error=checkbox.checked;}
        await api('/runs/'+encodeURIComponent(selected)+'/reconcile/'+encodeURIComponent(step.call.id),'POST',decision);
        message('Reconciliation saved. Resume the parent to continue.');await refresh();
      });form.append(button);
    }
    region.append(form);
  }
}
function childButton(owner,label){
  const button=document.createElement('button');button.className='secondary';button.textContent=label;
  button.onclick=()=>act(async()=>{childOwner=owner;selected=owner.run;$('sources').classList.add('hidden');await detailRun();$('selected').scrollIntoView({behavior:'smooth',block:'start'});});
  return button;
}
async function renderParents(){
  const [workflows,sessions,roles]=await Promise.all([rawApi('/workflows'),rawApi('/sessions'),rawApi('/roles')]);
  show('role-contracts',roles.roles);
  const signature=JSON.stringify([workflows,sessions,parentSelection]);
  if(signature!==parentSignature){
    const list=$('parents');list.replaceChildren();
    for(const [kind,items]of [['workflow',workflows.workflows],['session',sessions.sessions]])for(const item of items){
      const button=document.createElement('button');button.className='run'+(parentSelection?.id===item.id&&parentSelection.kind===kind?' selected':'');
      const title=document.createElement('strong');title.textContent=item.id;
      const detail=document.createElement('small');detail.textContent=kind+' · '+(item.status||(item.active?'active turn':'ready'))+' · '+(item.job.active?'worker active':'worker stopped');button.append(title,detail);
      button.onclick=()=>act(async()=>{parentSelection={kind,id:item.id};await renderParents();});list.append(button);
    }
    if(!list.children.length)list.textContent='No workflows or sessions yet.';
    parentSignature=signature;
  }
  if(!parentSelection)return;
  const data=await rawApi(parentPath(parentSelection));
  const state=data.state||data.session;show('parent-title',parentSelection.kind+' · '+state.id);show('parent-state',state);show('parent-error',data.job.error||'');
  $('parent-controls').classList.remove('hidden');
  $('parent-resume').disabled=data.job.active||(!data.state&&!state.active)||!!data.state&&['completed','failed','cancelled','deadline','dispatch_limit','budget_limit'].includes(state.status);
  $('parent-pause').disabled=!data.job.active;
  $('session-abandon').classList.toggle('hidden',parentSelection.kind!=='session'||!state.active);
  show('parent-budget',data.budget?JSON.stringify(data.budget):state.turns.length+' committed turns · '+(state.active?'active '+state.active.turn_id:'ready for next turn'));
  const nodes=$('parent-nodes');nodes.replaceChildren();
  if(data.state){
    for(const node of state.nodes){
      const row=document.createElement('div');const spec=state.definition.nodes.find(n=>n.id===node.id);
      const label=document.createElement('p');label.textContent=node.id+' · role '+spec.task+' · '+node.status+' ← '+(spec.depends_on.join(', ')||'input')+' · '+node.cost_micros+' µunits'+(node.uncertain_micros?' ('+node.uncertain_micros+' uncertain)':'');row.append(label);
      if(node.detail){const detail=document.createElement('p');detail.className='error';detail.textContent=node.detail;row.append(detail);}
      if(node.output!==undefined){const output=document.createElement('pre');output.textContent=JSON.stringify(node.output,null,2);row.append(output);}
      if(node.child)row.append(childButton({kind:'workflow',id:state.id,node:node.id,run:node.child.run_id,editable:true},'Inspect child · approvals / recovery'));
      nodes.append(row);
    }
  }else{
    for(const turn of [...state.turns.map(t=>({binding:t.binding,outcome:t.outcome,editable:false})),...(state.active?[{binding:state.active,outcome:'active',editable:true}]:[])]){
      const row=document.createElement('p');row.textContent=turn.binding.turn_id+' · '+turn.outcome+' · '+turn.binding.input;
      row.append(childButton({kind:'session',id:state.id,run:turn.binding.run_id,editable:turn.editable},'Inspect turn'));nodes.append(row);
    }
  }
}
const rawRefresh=refresh;
refresh=async function(){await rawRefresh();if(token){await renderParents();for(const button of $('runs').querySelectorAll('button'))if(button.querySelector('strong')?.textContent.length>64)button.remove();}};
$('parent-kind').onchange=()=>{$('dag-form').classList.toggle('hidden',$('parent-kind').value!=='workflow');$('session-send').classList.toggle('hidden',$('parent-kind').value!=='session');};$('parent-kind').onchange();
$('parent-create').onclick=()=>act(async()=>{
  const kind=$('parent-kind').value,id=$('parent-id').value;
  if(kind==='session')await rawApi('/sessions','POST',{id});
  else await rawApi('/workflows','POST',{id,input:$('parent-prompt').value,...(kind==='peer'?{peer:{planner:'planner',executor:'executor',writer:'writer',reviewer:'reviewer',max_tasks:2,max_revisions:1}}:{definition:TaskloomJson.parse($('dag-definition').value)})});
  parentSelection={kind:kind==='session'?'session':'workflow',id};message('Parent saved.');await refresh();
});
$('session-send').onclick=()=>act(async()=>{
  const id=$('parent-id').value;await rawApi('/sessions/'+encodeURIComponent(id)+'/turns','POST',{turn_id:'turn-'+Date.now(),prompt:$('parent-prompt').value});parentSelection={kind:'session',id};await refresh();message('Turn bound and checkpointed.');
});
for(const command of ['resume','pause','cancel'])$('parent-'+command).onclick=()=>act(async()=>{
  await rawApi(parentPath(parentSelection)+'/'+command,'POST',command==='cancel'?{actor:'local-operator',note:'cancelled from local console'}:undefined);await refresh();message('Parent '+command+' requested.');
});
$('session-abandon').onclick=()=>act(async()=>{
  await rawApi(parentPath(parentSelection)+'/abandon','POST',{actor:'local-operator',note:'Operator acknowledged failed terminal turn from console; inspect saved child evidence.'});await refresh();message('Failed terminal turn acknowledged. A new turn can now begin.');
});
