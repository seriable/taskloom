const knowledgeSection=$('knowledge').parentElement;
const researchPanel=document.createElement('div');
researchPanel.innerHTML=`<p id="research-capabilities" class="muted">PDF and web sources need host configuration.</p><label for="web-url">Web source URL</label><input id="web-url" type="url" placeholder="https://... (host-granted origin)"><button id="import-web" class="secondary">Import webpage</button><label for="web-query">Web search query</label><input id="web-query"><button id="web-search" class="secondary">Search web</button><div id="web-results"></div>`;
knowledgeSection.append(researchPanel);
const retrievalPanel=document.createElement('div');
retrievalPanel.innerHTML=`<h3>Local retrieval and embedding index</h3><p class="muted">Settings are host-owned. Validate and export this JSON as the retrieval property in your configuration file, then restart the host. Existing runs keep their frozen tool contract. Embedding builds and hybrid queries can incur separate charges.</p><label for="retrieval-config">Retrieval configuration (environment variable names only)</label><textarea id="retrieval-config" rows="12"></textarea><button id="validate-retrieval" class="secondary">Validate and export settings</button><button id="refresh-index" class="secondary">Refresh index status</button><button id="index-build" class="secondary">Build / update index using displayed configuration</button><pre id="index-status"></pre><label for="retrieval-query">Knowledge query</label><input id="retrieval-query"><button id="explain-retrieval" class="secondary">Explain query (no provider call)</button><button id="search-knowledge" class="secondary">Search using active retriever (may incur embedding charges)</button><pre id="retrieval-explanation"></pre>`;
knowledgeSection.append(retrievalPanel);
let retrievalStatus=null;
let retrievalDraftChanged=false;
$('retrieval-config').oninput=()=>{retrievalDraftChanged=true;};
async function refreshRetrieval(){
  retrievalStatus=await api('/retrieval');
  if(!retrievalDraftChanged)$('retrieval-config').value=JSON.stringify(retrievalStatus.configuration,null,2);
  $('index-build').disabled=!retrievalStatus.configuration.embedding;
  show('index-status',retrievalStatus);
}
$('refresh-index').onclick=()=>act(refreshRetrieval);
$('validate-retrieval').onclick=()=>act(async()=>{
  const config=JSON.parse($('retrieval-config').value);
  const checked=await api('/retrieval/validate','POST',config);
  const blob=new Blob([JSON.stringify({retrieval:checked.configuration},null,2)],{type:'application/json'});
  const url=URL.createObjectURL(blob);const link=document.createElement('a');link.href=url;link.download='taskloom-retrieval.json';link.click();URL.revokeObjectURL(url);
  message(checked.next_step);
});
$('index-build').onclick=()=>act(async()=>{
  if(!retrievalStatus)throw Error('Refresh index status first.');
  const checked=await api('/retrieval/validate','POST',JSON.parse($('retrieval-config').value));
  if(checked.configuration_binding!==retrievalStatus.configuration_binding)throw Error('Apply edited settings to the host configuration and restart before building.');
  await api('/retrieval/index','POST',{expected_revision:retrievalStatus.corpus_revision,expected_binding:retrievalStatus.configuration_binding});
  message('Index saved. Restart the host to load this index; service charges are recorded separately.');await refreshRetrieval();
});
$('explain-retrieval').onclick=()=>act(async()=>{
  show('retrieval-explanation',await api('/retrieval/explain','POST',{text:$('retrieval-query').value,top_k:5,max_bytes:24576}));
});
$('search-knowledge').onclick=()=>act(async()=>{
  show('retrieval-explanation',await api('/knowledge/search','POST',{text:$('retrieval-query').value,top_k:5,max_bytes:24576}));
  await refreshRetrieval();
});
$('file').setAttribute('accept','.txt,.md,.pdf,text/plain,text/markdown,application/pdf');
document.querySelector('label[for="file"]').textContent='Text / Markdown / PDF';
const textImport=$('import').onclick;
$('import').onclick=()=>{
  const file=$('file').files[0];
  if(!file||!file.name.toLowerCase().endsWith('.pdf'))return textImport();
  act(async()=>{
    if(file.size>750000)throw Error('Use the CLI for files over 750 KB.');
    const bytes=new Uint8Array(await file.arrayBuffer());let binary='';
    for(let offset=0;offset<bytes.length;offset+=16384)binary+=String.fromCharCode(...bytes.subarray(offset,offset+16384));
    await api('/knowledge/pdf','POST',{id:$('doc-id').value,title:$('doc-title').value,filename:file.name,data_base64:btoa(binary),expected_revision:null});message('PDF text imported with source hash and page separators.');await refresh();
  });
};
$('import-web').onclick=()=>act(async()=>{
  await api('/knowledge/web','POST',{id:$('doc-id').value,title:$('doc-title').value,url:$('web-url').value,expected_revision:null});message('Extracted webpage imported with source URL and body hash.');await refresh();
});
$('web-search').onclick=()=>act(async()=>{
  const data=await api('/research/search','POST',{query:$('web-query').value,count:5});$('web-results').replaceChildren();
  for(const hit of data.results){
    const entry=document.createElement('div');const title=document.createElement('p');title.textContent=hit.title;
    const snippet=document.createElement('p');snippet.textContent=hit.snippet;const url=document.createElement('p');url.textContent=hit.url;
    const use=document.createElement('button');use.className='secondary';use.textContent='Use source URL';use.onclick=()=>{$('web-url').value=hit.url;$('doc-title').value=hit.title;message('URL selected. Choose a document ID and import only if the origin is granted.');};entry.append(title,snippet,url,use);$('web-results').append(entry);
  }
  message('Search snippets are unverified leads. Import and inspect original sources before citing.');
});
const refreshBeforeResearch=refresh;
refresh=async function(){
  await refreshBeforeResearch();if(!token)return;
  const capabilities=await api('/research/capabilities');
  if(!retrievalStatus)await refreshRetrieval();
  $('import-web').disabled=!capabilities.web;$('web-search').disabled=!capabilities.search;
  show('research-capabilities','PDF '+(capabilities.pdf?'enabled':'not configured')+' · Web '+(capabilities.web?'enabled for '+capabilities.allowed_origins.join(', '):'not configured')+' · Search '+(capabilities.search?'enabled':'not configured'));
};
