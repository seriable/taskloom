const knowledgeSection=$('knowledge').parentElement;
const researchPanel=document.createElement('div');
researchPanel.innerHTML=`<p id="research-capabilities" class="muted">PDF and web sources need host configuration.</p><label for="web-url">Web source URL</label><input id="web-url" type="url" placeholder="https://... (host-granted origin)"><button id="import-web" class="secondary">Import webpage</button><label for="web-query">Web search query</label><input id="web-query"><button id="web-search" class="secondary">Search web</button><div id="web-results"></div>`;
knowledgeSection.append(researchPanel);
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
  $('import-web').disabled=!capabilities.web;$('web-search').disabled=!capabilities.search;
  show('research-capabilities','PDF '+(capabilities.pdf?'enabled':'not configured')+' · Web '+(capabilities.web?'enabled for '+capabilities.allowed_origins.join(', '):'not configured')+' · Search '+(capabilities.search?'enabled':'not configured'));
};
