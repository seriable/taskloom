const diagnosticStyle=document.createElement('style');
diagnosticStyle.textContent='.diagnostic-table{min-width:640px}.diagnostic-table th:first-child{width:16%}.diagnostic-table th:nth-child(2){width:16%}.diagnostic-table th:nth-child(3){width:16%}.comparison-table{min-width:680px}.comparison-table th:first-child{width:18%}.comparison-table th:nth-child(2){width:23%}.comparison-table th:nth-child(3){width:23%}';
document.head.append(diagnosticStyle);
const diagnosticSection=document.createElement('section');
diagnosticSection.innerHTML=`<h2>Trace and billing diagnostics</h2><p class="muted">Inspect saved journals without model or tool calls. Trace/span IDs are pseudonymous; saved evidence metadata stays local. Parent/child reads are individually atomic; a running workflow can change between reads. Missing children and clocks remain visible.</p><div class="row"><div><label for="diagnostic-kind">Record type</label><select id="diagnostic-kind"><option value="run">Run</option><option value="workflow">Workflow</option><option value="session">Session</option></select></div><div><label for="diagnostic-id">Record ID (blank uses selected run)</label><input id="diagnostic-id"></div></div><button id="load-diagnostics" class="secondary">Inspect trace</button><button id="export-diagnostics" class="secondary" disabled>Export displayed journal revisions</button><p id="diagnostic-summary" class="muted">Export is off unless a destination is configured by the host. Only the export button sends data.</p><div style="overflow:auto"><table class="diagnostic-table"><thead><tr><th>Kind</th><th>Outcome</th><th>Active ms</th><th>Clock / span / parent</th></tr></thead><tbody id="diagnostic-spans"></tbody></table></div><details><summary>Metrics, retrieval and error attribution</summary><pre id="diagnostic-metrics"></pre></details><pre id="diagnostic-export-result" class="hidden"></pre>`;
document.querySelector('main').append(diagnosticSection);
let displayedDiagnostic=null;
for(const name of ['diagnostic-kind','diagnostic-id'])$(name).oninput=()=>{displayedDiagnostic=null;$('export-diagnostics').disabled=true;};
$('load-diagnostics').onclick=()=>act(async()=>{
  const kind=$('diagnostic-kind').value;const id=$('diagnostic-id').value.trim()||(kind==='run'?selected:'');
  if(!id)throw Error('Choose a record ID.');
  const data=await api('/diagnostics/'+kind+'/'+encodeURIComponent(id));
  displayedDiagnostic={kind,id,key:data.snapshot.source_revision_key,binding:data.export_binding};
  $('export-diagnostics').disabled=!data.export_configured;
  show('diagnostic-summary','Trace '+data.snapshot.trace_id+' · '+data.snapshot.spans.length+' spans · '+data.snapshot.missing_children+' missing children. '+(data.export_configured?'Export destination: '+data.export_destination:'Export is not configured.'));
  $('diagnostic-spans').replaceChildren();
  for(const span of data.snapshot.spans){
    const row=document.createElement('tr');
    const clock=!span.ended?'open':span.start_unix_ms==null||span.end_unix_ms==null?'wall clock unavailable':span.end_unix_ms<span.start_unix_ms?'wall clock moved backwards':'recorded wall clock';
    for(const value of [span.kind,span.outcome,span.active_elapsed_ms??'unknown',clock+'\n'+span.span_id+'\nparent: '+(span.parent_span_id??'root')]){const cell=document.createElement('td');cell.textContent=value;row.append(cell);}
    $('diagnostic-spans').append(row);
  }
  show('diagnostic-metrics',{metrics:data.snapshot.metrics,billing_boundaries:data.billing_boundaries,retrievals:data.snapshot.retrievals,issues:data.snapshot.issues});
  $('diagnostic-export-result').classList.add('hidden');message('Loaded saved trace; nothing exported.');
});
$('export-diagnostics').onclick=()=>act(async()=>{
  if(!displayedDiagnostic)throw Error('Inspect the trace first.');
  const {kind,id,key,binding}=displayedDiagnostic;$('export-diagnostics').disabled=true;
  try {
    const receipt=await api('/diagnostics/'+kind+'/'+encodeURIComponent(id)+'/export','POST',{source_revision_key:key,export_binding:binding});
    show('diagnostic-export-result',receipt);$('diagnostic-export-result').classList.remove('hidden');
    message('Export attempt recorded. Inspect both receiver statuses and omitted-span counts; failed or unknown sends are not retried.');
  } finally {displayedDiagnostic=null;}
});

const comparisonSection=document.createElement('section');
comparisonSection.innerHTML=`<h2>Evaluation comparison</h2><p class="muted">Load two version-1 Taskloom benchmark reports locally. Files are not uploaded or independently verified. Results are descriptive; matching labels alone do not prove equal prices, hardware, prompts or independent quality. Recorded generation ledgers exclude embedding and tool services. Fixture/offline ledgers do not measure actual API/chat billing; their wall time is integration time.</p><div class="row"><div><label for="comparison-left">First benchmark report</label><input id="comparison-left" type="file" accept="application/json,.json"></div><div><label for="comparison-right">Second benchmark report</label><input id="comparison-right" type="file" accept="application/json,.json"></div></div><button id="compare-evaluations" class="secondary">Compare reports locally</button><pre id="comparison-conditions"></pre><div style="overflow:auto"><table class="comparison-table"><thead><tr><th>Case</th><th>First</th><th>Second</th><th>Failure reasons</th></tr></thead><tbody id="comparison-cases"></tbody></table></div>`;
document.querySelector('main').append(comparisonSection);
for(const name of ['comparison-left','comparison-right'])$(name).onchange=()=>{show('comparison-conditions','Report selection changed; compare again.');$('comparison-cases').replaceChildren();};
async function comparisonReport(input){
  const file=$(input).files[0];if(!file||file.size>4*1024*1024)throw Error('Choose a benchmark JSON file no larger than 4 MiB.');
  const report=JSON.parse(await file.text());
  if(report.version!==1||!report.origin||!report.configuration||!['configured_provider','fixture','offline_demo'].includes(report.origin.kind)||typeof report.origin.model!=='string'||!/^[a-f0-9]{64}$/.test(report.suite_fingerprint)||!Array.isArray(report.cases)||report.cases.length>256)throw Error('Expected a version-1 Taskloom BenchmarkReport; other campaign formats need an explicit adapter.');
  const ids=new Set();
  for(const c of report.cases){if(typeof c.id!=='string'||ids.has(c.id)||!['passed','failed','running','not_run'].includes(c.status)||!Array.isArray(c.failures)||c.failures.some(f=>typeof f!=='string'))throw Error('Invalid/duplicate benchmark case.');ids.add(c.id);}
  return {name:file.name,report};
}
function canonicalDiagnostic(value){if(Array.isArray(value))return value.map(canonicalDiagnostic);if(value&&typeof value==='object')return Object.fromEntries(Object.keys(value).sort().map(k=>[k,canonicalDiagnostic(value[k])]));return value;}
$('compare-evaluations').onclick=()=>act(async()=>{
  const [left,right]=await Promise.all([comparisonReport('comparison-left'),comparisonReport('comparison-right')]);
  const reasons=[];
  for(const [label,a,b]of [['suite fingerprint',left.report.suite_fingerprint,right.report.suite_fingerprint],['model',left.report.origin.model,right.report.origin.model],['execution kind',left.report.origin.kind,right.report.origin.kind],['execution configuration',left.report.configuration,right.report.configuration],['success definition',left.report.success_definition,right.report.success_definition]])if(JSON.stringify(canonicalDiagnostic(a))!==JSON.stringify(canonicalDiagnostic(b)))reasons.push(label+' differs');
  const summary=entry=>({file:entry.name,origin:entry.report.origin,completed:entry.report.completed,passed:entry.report.cases.filter(c=>c.status==='passed').length,requested:entry.report.cases.length,wall_elapsed_ms:entry.report.wall_elapsed_ms,recorded_generation_ledger_microunits:entry.report.charged_model_cost_micros,measurement:entry.report.origin.kind==='configured_provider'?'declared native-provider harness; prices and costs require source verification':'fixture/offline integration; recorded ledger is not actual API/chat billing and wall time is not inference latency',uncertain_microunits:entry.report.uncertain_model_cost_micros,unknown_cost_cases:entry.report.unknown_cost_cases});
  show('comparison-conditions',{eligibility:'descriptive_only',declared_condition_differences:reasons,first:summary(left),second:summary(right),limits:'Source files and claim-support labels are not independently verified. No superiority or latency ranking is inferred.'});
  $('comparison-cases').replaceChildren();
  const leftCases=new Map(left.report.cases.map(c=>[c.id,c]));const rightCases=new Map(right.report.cases.map(c=>[c.id,c]));
  for(const id of new Set([...leftCases.keys(),...rightCases.keys()])){const a=leftCases.get(id),b=rightCases.get(id);const row=document.createElement('tr');const detail=c=>c?c.status+' · '+c.model_steps+' model steps · '+c.wall_elapsed_ms+' ms · recorded ledger '+c.charged_model_cost_micros+' µunits'+(c.cost_unknown?' (cost unknown)':''):'missing';for(const value of [id,detail(a),detail(b),JSON.stringify({first:a?.failures??['missing case'],second:b?.failures??['missing case']})]){const cell=document.createElement('td');cell.textContent=value;row.append(cell);}$('comparison-cases').append(row);}
  message('Compared local report contents. Differences and missing cases remain visible.');
});
