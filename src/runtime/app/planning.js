// Host-authorized configuration review and exact workflow authoring.
// Dynamic/model/source text only enters textContent or form values.
(() => {
  'use strict';
  const codec = TaskloomJson, parse = codec.parse, encode = codec.stringify;
  const clone = value => parse(encode(value));
  const root = document.createElement('section');
  root.id = 'design-panel';
  root.innerHTML = `<h2>Configure & design</h2>
<p class="muted">Review a new host configuration, or build a workflow from roles already granted by the running host. Configuration export takes effect after an explicit host restart.</p>
<button id="design-load" class="secondary">Load host capabilities</button>
<details id="config-wizard"><summary>Configuration wizard</summary>
<div class="row"><div><label for="config-file">Import existing configuration</label><input id="config-file" type="file" accept=".json,application/json"></div><button id="config-defaults" class="secondary">Start from offline defaults</button></div>
<p class="muted">Use credential environment names only. Configure their values on the host. Keep advanced fields in the complete JSON below.</p>
<fieldset><legend>1 · Model & budget</legend><div class="row"><div><label for="config-state">State directory</label><input id="config-state"></div><div><label for="config-provider">Provider</label><select id="config-provider"><option value="offline">Offline excerpts</option><option value="openai">OpenAI compatible</option><option value="anthropic">Anthropic compatible</option><option value="native:responses">OpenAI Responses</option><option value="native:gemini">Gemini native</option><option value="native:vertex">Vertex native (host bearer)</option><option value="native:bedrock">Bedrock Converse (host bearer)</option><option value="native:azure">Azure deployment</option></select></div><div><label for="config-model">Model name</label><input id="config-model"></div></div>
<div class="row"><div><label for="config-endpoint">Model API URL</label><input id="config-endpoint" placeholder="https://api.example.com/v1"></div><div><label for="config-key">Credential environment name</label><input id="config-key" placeholder="MODEL_API_KEY" autocomplete="off"></div><div><label for="config-namespace">Provider namespace</label><input id="config-namespace"></div></div>
<div class="row"><div><label for="config-cap">Generation cap (USD micro-units, or null)</label><input id="config-cap"></div><div><label for="config-output">Output tokens per request</label><input id="config-output"></div><div><label for="config-price-input">USD / million input tokens</label><input id="config-price-input"></div><div><label for="config-price-output">USD / million output tokens</label><input id="config-price-output"></div></div></fieldset>
<fieldset><legend>2 · Tools, roles & retrieval</legend><label><input id="config-knowledge" type="checkbox" style="width:auto"> Offer local knowledge search</label>
<label for="config-mcp">MCP declarations (endpoint, credential names, bindings, effects and permissions)</label><textarea id="config-mcp" rows="6"></textarea>
<label for="config-roles">Host role contracts (system, tools and output contract)</label><textarea id="config-roles" rows="6"></textarea>
<label for="config-retrieval">Retrieval / embedding configuration</label><textarea id="config-retrieval" rows="6"></textarea>
<button id="config-fields" class="secondary">Update complete JSON from these fields</button></fieldset>
<fieldset><legend>3 · Review & export</legend><label for="config-source">Complete configuration JSON</label><textarea id="config-source" rows="12" spellcheck="false"></textarea>
<button id="config-source-fields" class="secondary">Load fields from JSON</button><button id="config-check">Review proposed configuration</button><button id="config-export" class="secondary" disabled>Export reviewed configuration</button>
<p id="config-message" role="status" class="muted"></p><pre id="config-review"></pre></fieldset></details>
<h3>Workflow editor</h3><div class="row"><div><label for="design-template">Starting point</label><select id="design-template"><option value="blank">Blank role DAG</option><option value="peer">PEER roles</option></select></div><button id="design-template-use" class="secondary">Use starting point</button></div>
<pre id="design-template-contract" class="hidden"></pre>
<div class="row"><div><label for="design-id">New workflow ID</label><input id="design-id"></div><div><label for="design-import">Import workflow request JSON</label><input id="design-import" type="file" accept=".json,application/json"></div></div>
<label for="design-input">Task input as JSON (a quoted string or structured object)</label><textarea id="design-input" rows="3"></textarea>
<label for="design-limits">Workflow limits</label><textarea id="design-limits" rows="4"></textarea>
<p class="muted">Each connection declares a dependency. Conditions and selectors may only reference declared dependencies. Removing or renaming nodes leaves references visible for correction; validation reports dangling bindings.</p>
<div id="design-graph" class="workflow-graph" aria-label="Workflow dependency diagram"></div>
<button id="design-add" class="secondary">Add node</button><div id="design-nodes"></div>
<details><summary>Code representation</summary><label for="design-source">Exact workflow request JSON</label><textarea id="design-source" rows="14" spellcheck="false"></textarea><button id="design-source-use" class="secondary">Load code into editor</button><button id="design-export" class="secondary">Export workflow JSON</button></details>
<button id="design-preview">Validate & preview workflow</button><button id="design-launch" disabled>Launch reviewed workflow</button>
<p id="design-message" role="status" class="muted">Connect and load host capabilities to begin.</p><pre id="design-preview-state" class="hidden"></pre>`;
  document.querySelector('main>.grid').before(root);
  const el = id => document.getElementById(id);
  let setup = null, configReview = null, configReviewedText = '', draft = null, reviewed = null;
  let template = null, sourceDirty = false, configFieldsDirty = false, revision = 0;
  const pendingFields = new Map();
  function note(id, text, error = false) { el(id).textContent = text; el(id).className = error ? 'error' : 'muted'; }
  function action(work, id = 'design-message') {
    return async () => { try { await work(); } catch (error) { note(id, error.message, true); } };
  }
  async function exactApi(path, body) {
    if (!token) throw Error('Enter the API token and connect.');
    const response = await fetch('/api' + path, {method: body === undefined ? 'GET' : 'POST',
      headers: {Authorization: 'Bearer ' + token, ...(body === undefined ? {} : {'Content-Type': 'application/json'})},
      body: body === undefined ? undefined : encode(body)});
    const text = await response.text();
    let value; try { value = parse(text); } catch { throw Error('HTTP ' + response.status + ': invalid JSON response'); }
    if (!response.ok) throw Error(value.error || 'HTTP ' + response.status);
    return value;
  }
  function download(filename, text) {
    const url = URL.createObjectURL(new Blob([text + '\n'], {type: 'application/json'}));
    const link = document.createElement('a'); link.href = url; link.download = filename; link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  async function fileText(input) {
    const file = input.files[0]; if (!file) throw Error('Choose a JSON file.');
    if (file.size > 1024 * 1024) throw Error('Use a JSON file no larger than 1 MiB.');
    return new TextDecoder('utf-8', {fatal: true}).decode(await file.arrayBuffer());
  }
  function configInvalidated() {
    configReview = null; configReviewedText = ''; el('config-export').disabled = true;
    el('config-review').textContent = ''; note('config-message', 'Configuration changed. Review again before export.');
  }
  function configFields(value) {
    el('config-state').value = value.state_dir;
    el('config-provider').value = value.provider.type === 'native' ? 'native:' + value.provider.protocol : value.provider.type;
    el('config-model').value = value.model;
    el('config-endpoint').value = value.provider.base_url || '';
    el('config-key').value = value.provider.key_env || '';
    el('config-namespace').value = value.provider.namespace || '';
    el('config-cap').value = encode(value.limits.max_cost_micros);
    el('config-output').value = String(value.output_tokens);
    const prices = value.prices.find(p => p.prefix === value.model);
    el('config-price-input').value = prices ? String(prices.input_per_million) : '';
    el('config-price-output').value = prices ? String(prices.output_per_million) : '';
    el('config-knowledge').checked = value.knowledge;
    el('config-mcp').value = encode(value.mcp, true);
    el('config-roles').value = encode(value.roles, true);
    el('config-retrieval').value = encode(value.retrieval, true);
    configFieldsDirty = false;
  }
  function setConfig(value) {
    configFields(value); el('config-source').value = encode(value, true); configInvalidated();
  }
  function updateConfig() {
    const value = parse(el('config-source').value), type = el('config-provider').value;
    value.state_dir = el('config-state').value; value.model = el('config-model').value;
    value.provider = type === 'offline' ? {type} : {type, base_url: el('config-endpoint').value,
      key_env: el('config-key').value, namespace: el('config-namespace').value};
    if (type.startsWith('native:')) { value.provider.type = 'native'; value.provider.protocol = type.slice(7); }
    value.limits.max_cost_micros = parse(el('config-cap').value);
    value.output_tokens = parse(el('config-output').value);
    value.knowledge = el('config-knowledge').checked; value.mcp = parse(el('config-mcp').value);
    value.roles = parse(el('config-roles').value); value.retrieval = parse(el('config-retrieval').value);
    if (type !== 'offline') {
      const price = {prefix: value.model, input_per_million: parse(el('config-price-input').value),
        output_per_million: parse(el('config-price-output').value)};
      value.prices = [price, ...value.prices.filter(p => p.prefix !== value.model)];
    }
    el('config-source').value = encode(value, true); configFieldsDirty = false; configInvalidated();
    note('config-message', 'Fields copied to complete configuration. Review before export.');
  }
  for (const field of root.querySelectorAll('#config-wizard input:not([type=file]), #config-wizard select, #config-wizard textarea')) {
    field.addEventListener('input', () => { if (field.id !== 'config-source') configFieldsDirty = true; configInvalidated(); });
  }
  el('config-defaults').onclick = action(() => { if (!setup) throw Error('Load host capabilities first.'); setConfig(clone(setup.defaults)); }, 'config-message');
  el('config-file').onchange = action(async () => setConfig(parse(await fileText(el('config-file')))), 'config-message');
  el('config-source-fields').onclick = action(() => { configFields(parse(el('config-source').value)); configInvalidated(); }, 'config-message');
  el('config-fields').onclick = action(updateConfig, 'config-message');
  el('config-check').onclick = action(async () => {
    if (configFieldsDirty) throw Error('Update complete JSON from edited fields before review.');
    configInvalidated();
    const source = el('config-source').value;
    const result = await exactApi('/setup/validate', parse(source));
    if (el('config-source').value !== source || configFieldsDirty) throw Error('Configuration changed during review. Review again.');
    configReview = result; configReviewedText = source;
    el('config-review').textContent = encode({credential_names: result.credential_names, declared_tools: result.declared_tools,
      deferred_checks: result.deferred_checks, services_contacted: result.services_contacted, configuration_applied: result.configuration_applied}, true);
    el('config-export').disabled = false;
    note('config-message', 'Static review passed. Export, configure named credentials and restart explicitly. Live tools and grants are unchanged.');
  }, 'config-message');
  el('config-export').onclick = action(() => {
    if (!configReview || configFieldsDirty || el('config-source').value !== configReviewedText) throw Error('Review the current configuration before export.');
    download('taskloom-reviewed.json', encode(configReview.configuration, true));
  }, 'config-message');
  function invalidate(text = 'Workflow changed. Validate and preview before launch.') {
    revision++; reviewed = null; el('design-launch').disabled = true;
    el('design-preview-state').textContent = ''; el('design-preview-state').classList.add('hidden'); note('design-message', text);
  }
  function requireDraft() { if (!draft) throw Error('Load host capabilities and choose a starting point.'); }
  function sync() {
    requireDraft(); if (sourceDirty) throw Error('Load edited code into the editor before continuing.');
    draft.id = el('design-id').value; draft.input = parse(el('design-input').value); draft.limits = parse(el('design-limits').value);
    // Commit the latest visible values even when synthetic fill/input or an IME
    // has not emitted change/blur. An invalid value blocks preview and launch.
    if (pendingFields.size) template = null;
    for (const [field, commit] of pendingFields) { commit(field.value); pendingFields.delete(field); }
    el('design-source').value = encode(draft, true);
    return draft;
  }
  function acceptDraft(value, selectedTemplate = null) {
    if (!value || typeof value !== 'object' || typeof value.id !== 'string' || !Object.hasOwn(value, 'input') ||
        !value.limits || !(value.definition?.nodes instanceof Array) && !value.peer) throw Error('Import a complete workflow request with limits and a definition or PEER spec.');
    if (value.definition && value.definition.nodes.some(node => typeof node.id !== 'string' || typeof node.task !== 'string' ||
        !Array.isArray(node.depends_on) || !node.depends_on.every(id => typeof id === 'string') || !node.condition || typeof node.condition.type !== 'string' ||
        !node.select || typeof node.select !== 'object' || Array.isArray(node.select) || Object.values(node.select).some(b => !b || typeof b.node !== 'string' || typeof b.pointer !== 'string') || !Object.hasOwn(node,'params'))) {
      throw Error('Each node needs id, task, dependencies, condition, select bindings and params. Keep advanced values as JSON.');
    }
    draft = value; template = selectedTemplate; sourceDirty = false; pendingFields.clear();
    el('design-id').value = value.id; el('design-input').value = encode(value.input, true); el('design-limits').value = encode(value.limits, true);
    el('design-source').value = encode(draft, true); invalidate(); render();
  }
  function edit(work) {
    return action(() => { sync(); work(); template = null; invalidate(); el('design-source').value = encode(draft, true); render(); });
  }
  function field(label, value, callback, multiline = false) {
    const wrap = document.createElement('label'); wrap.textContent = label;
    const input = document.createElement(multiline ? 'textarea' : 'input'); input.value = value;
    input.setAttribute('aria-label', label); if (multiline) input.rows = 3;
    input.oninput = () => { pendingFields.set(input, callback); invalidate(); }; input.onchange = edit(() => {});
    wrap.append(input); return wrap;
  }
  function choices(label, values, selected, callback, multiple = false) {
    const wrap = document.createElement('label'); wrap.textContent = label;
    const input = document.createElement('select'); input.multiple = multiple; input.setAttribute('aria-label', label);
    const all = [...new Set([...values, ...selected])];
    if (!multiple && !all.length) all.push('');
    for (const value of all) { const option = new Option(value || '(choose a registered role)', value); option.selected = selected.includes(value); input.add(option); }
    input.onchange = edit(() => callback([...input.selectedOptions].map(o => o.value)));
    wrap.append(input); return wrap;
  }
  function conditionEditor(node, region) {
    const condition = node.condition;
    const simple = ['always', 'exists', 'equals'].includes(condition?.type);
    region.append(choices('Condition type · ' + node.id, ['always', 'exists', 'equals', 'advanced'], [simple ? condition.type : 'advanced'], values => {
      const type = values[0];
      if (type === 'advanced') { if (simple) node.condition = {type:'all', conditions:[clone(condition)]}; }
      else node.condition = type === 'always' ? {type} : {type, node:node.depends_on[0] || '', pointer:'', ...(type === 'equals' ? {value:null} : {})};
    }));
    if (!simple) { region.append(field('Advanced condition JSON · ' + node.id, encode(condition, true), v => { node.condition = parse(v); }, true)); return; }
    if (condition.type !== 'always') {
      region.append(choices('Condition dependency · ' + node.id, node.depends_on, [condition.node], values => { condition.node = values[0]; }));
      region.append(field('Condition JSON pointer · ' + node.id, condition.pointer, v => { condition.pointer = v; }));
      if (condition.type === 'equals') region.append(field('Expected JSON value · ' + node.id, encode(condition.value), v => { condition.value = parse(v); }));
    }
  }
  function render() {
    el('design-nodes').replaceChildren(); el('design-add').disabled = !draft?.definition;
    if (!draft) return;
    if (!draft.definition) {
      el('design-graph').textContent = 'PEER expands at preview into planner → parallel execution → writer → reviewer, with bounded revision branches.';
      el('design-nodes').append(field('PEER specification JSON', encode(draft.peer, true), v => { draft.peer = parse(v); }, true)); return;
    }
    const nodes = draft.definition.nodes, ids = nodes.map(n => n.id), roles = (setup?.active_roles || []).map(r => r.name);
    drawGraph(nodes);
    for (const node of nodes) {
      const card = document.createElement('fieldset'); card.className = 'workflow-node';
      const title = document.createElement('legend'); title.textContent = node.id || '(unnamed node)'; card.append(title);
      card.append(field('Node ID · ' + node.id, node.id, v => { node.id = v; }));
      card.append(choices('Registered role · ' + node.id, roles, [node.task], values => { node.task = values[0]; }));
      if (!roles.includes(node.task)) { const warning = document.createElement('p'); warning.className = 'error'; warning.textContent = 'This role is not registered in the active host. Select or configure it explicitly.'; card.append(warning); }
      card.append(choices('Dependencies · ' + node.id, ids.filter(id => id !== node.id), node.depends_on, values => { node.depends_on = values; }, true));
      conditionEditor(node, card);
      const optional = document.createElement('label'), check = document.createElement('input'); check.type = 'checkbox'; check.style.width = 'auto'; check.checked = node.allow_skipped_dependencies;
      check.onchange = edit(() => { node.allow_skipped_dependencies = check.checked; }); optional.append(check, ' Allow skipped dependencies · ' + node.id); card.append(optional);
      card.append(field('Parameters JSON · ' + node.id, encode(node.params, true), v => { node.params = parse(v); }, true));
      const selectTitle = document.createElement('p'); selectTitle.textContent = 'Selected handoffs · empty selects all completed dependencies'; card.append(selectTitle);
      for (const [alias, binding] of Object.entries(node.select)) {
        const row = document.createElement('div'); row.className = 'row';
        row.append(field('Handoff alias · ' + node.id + ' · ' + alias, alias, value => {
          if (!value || value !== alias && Object.hasOwn(node.select, value)) throw Error('Use a nonempty, unique handoff alias.');
          delete node.select[alias]; Object.defineProperty(node.select, value, {value:binding, enumerable:true, configurable:true, writable:true});
        }));
        row.append(choices('Handoff dependency · ' + node.id + ' · ' + alias, node.depends_on, [binding.node], values => { binding.node = values[0]; }));
        row.append(field('Handoff pointer · ' + node.id + ' · ' + alias, binding.pointer, v => { binding.pointer = v; }));
        const remove = document.createElement('button'); remove.textContent = 'Remove handoff'; remove.className = 'secondary'; remove.onclick = edit(() => { delete node.select[alias]; }); row.append(remove); card.append(row);
      }
      const add = document.createElement('button'); add.textContent = 'Add handoff · ' + node.id; add.className = 'secondary'; add.onclick = edit(() => {
        let i = 1; while (Object.hasOwn(node.select, 'source' + i)) i++;
        node.select['source' + i] = {node:node.depends_on[0] || '', pointer:''};
      }); card.append(add);
      const remove = document.createElement('button'); remove.textContent = 'Remove node · ' + node.id; remove.className = 'danger'; remove.onclick = edit(() => { draft.definition.nodes = nodes.filter(n => n !== node); }); card.append(remove);
      el('design-nodes').append(card);
    }
  }
  function drawGraph(nodes) {
    const region = el('design-graph'); region.replaceChildren();
    if (!nodes.length) { region.textContent = 'Add a node and connect its dependencies.'; return; }
    const ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg');
    const positions = new Map(nodes.map((node, i) => [node.id, {x:20 + (i % 3) * 260, y:30 + Math.floor(i / 3) * 105}]));
    svg.setAttribute('viewBox', '0 0 800 ' + (Math.ceil(nodes.length / 3) * 105 + 25));
    svg.setAttribute('role', 'img'); svg.setAttribute('aria-label', 'Nodes and directed dependency connections');
    const defs = document.createElementNS(ns, 'defs'), marker = document.createElementNS(ns, 'marker');
    marker.id = 'workflow-arrow'; marker.setAttribute('markerWidth', '8'); marker.setAttribute('markerHeight', '8'); marker.setAttribute('refX', '7'); marker.setAttribute('refY', '3'); marker.setAttribute('orient', 'auto');
    const tip = document.createElementNS(ns, 'path'); tip.setAttribute('d', 'M0,0 L0,6 L8,3 z'); tip.setAttribute('fill', '#14667b'); marker.append(tip); defs.append(marker); svg.append(defs);
    for (const node of nodes) for (const dependency of node.depends_on) {
      const from = positions.get(dependency), to = positions.get(node.id); if (!from || !to) continue;
      const edge = document.createElementNS(ns, 'path'); edge.setAttribute('d', `M${from.x + 110},${from.y + 55} L${to.x + 110},${to.y}`);
      edge.setAttribute('stroke', '#14667b'); edge.setAttribute('fill', 'none'); edge.setAttribute('marker-end', 'url(#workflow-arrow)'); svg.append(edge);
    }
    for (const node of nodes) {
      const {x,y} = positions.get(node.id), rect = document.createElementNS(ns, 'rect');
      for (const [key, value] of Object.entries({x,y,width:220,height:55,rx:6,fill:'#e4f1f4',stroke:'#14667b'})) rect.setAttribute(key, String(value)); svg.append(rect);
      for (const [text, offset] of [[node.id,22],[node.task,43]]) {
        const label = document.createElementNS(ns, 'text'); label.setAttribute('x', String(x + 8)); label.setAttribute('y', String(y + offset)); label.textContent = text.length > 26 ? text.slice(0,23) + '…' : text; svg.append(label);
      }
    }
    region.append(svg);
    const legend = document.createElement('p'); legend.className = 'muted';
    legend.textContent = nodes.map(n => n.id + ' ← ' + (n.depends_on.join(', ') || 'task input')).join(' · '); region.append(legend);
  }
  async function load() {
    const result = await exactApi('/setup'); setup = result;
    el('design-template').replaceChildren(new Option('Blank role DAG', 'blank'), new Option('PEER roles', 'peer'));
    for (const [i, t] of setup.templates.templates.entries()) el('design-template').add(new Option(t.title + ' · ' + t.mount + '/' + t.id, 'template:' + i));
    if (!el('config-source').value) setConfig(clone(setup.defaults));
    invalidate('Host capabilities loaded. Choose a starting point or import a workflow.');
    if (draft) render();
  }
  el('design-load').onclick = action(load);
  el('design-template').onchange = () => {
    const option = el('design-template').value, t = option.startsWith('template:') ? setup?.templates.templates[Number(option.slice(9))] : null;
    el('design-template-contract').classList.toggle('hidden', !t); el('design-template-contract').textContent = t ? encode({package:t.package, description:t.description, input_schema:t.input_schema}, true) : '';
  };
  el('design-template-use').onclick = action(async () => {
    if (!setup) throw Error('Load host capabilities first.');
    const choice = el('design-template').value;
    const value = {id:'workflow-' + Date.now(), input:'Describe the task here.', definition:null, peer:null, limits:clone(setup.workflow_defaults)};
    if (choice.startsWith('template:')) {
      const t = setup.templates.templates[Number(choice.slice(9))];
      const compiled = clone(t.workflow), task = name => t.mount + '-' + name;
      if (compiled.type === 'dag') { value.definition = compiled.definition; for (const node of value.definition.nodes) node.task = task(node.task); }
      else { value.peer = compiled.spec; for (const key of ['planner','executor','writer','reviewer']) value.peer[key] = task(value.peer[key]); }
      value.input = {}; acceptDraft(value, t);
      note('design-message', 'Template loaded. Supply input matching its displayed schema. Editing graph contracts switches to a standalone role workflow.');
    } else if (choice === 'peer') {
      value.peer = {planner:'planner', executor:'executor', writer:'writer', reviewer:'reviewer', max_tasks:2, max_revisions:1}; acceptDraft(value);
    } else {
      value.definition = {nodes:[{id:'work', task:setup.active_roles[0]?.name || '', depends_on:[], condition:{type:'always'}, allow_skipped_dependencies:false, select:{}, params:null}]}; acceptDraft(value);
    }
  });
  el('design-add').onclick = edit(() => {
    let i = 1; const nodes = draft.definition.nodes; while (nodes.some(n => n.id === 'node' + i)) i++;
    if (nodes.length >= 256) throw Error('At most 256 nodes.');
    nodes.push({id:'node' + i, task:setup?.active_roles[0]?.name || '', depends_on:[], condition:{type:'always'}, allow_skipped_dependencies:false, select:{}, params:null});
  });
  for (const id of ['design-id','design-input','design-limits']) el(id).oninput = () => invalidate();
  el('design-source').oninput = () => { sourceDirty = true; invalidate('Code changed. Load code into the editor, then preview.'); };
  el('design-source-use').onclick = action(() => acceptDraft(parse(el('design-source').value)));
  el('design-import').onchange = action(async () => acceptDraft(parse(await fileText(el('design-import')))));
  el('design-export').onclick = action(() => download('workflow-request.json', encode(sync(), true)));
  el('design-preview').onclick = action(async () => {
    const request = clone(sync()); invalidate('Validating exact graph and frozen role contracts…'); const expected = revision;
    const result = template ? await exactApi('/templates/' + encodeURIComponent(template.mount) + '/' + encodeURIComponent(template.id) + '/preview',
      {id:request.id, input:request.input, limits:request.limits}) : await exactApi('/workflow-design/preview', request);
    if (revision !== expected || encode(sync()) !== encode(request)) throw Error('Workflow changed during validation. Preview again.');
    reviewed = {...result, editor_text:encode(request)}; el('design-preview-state').textContent = encode(result.plan, true); el('design-preview-state').classList.remove('hidden');
    el('design-launch').disabled = false;
    note('design-message', 'Preview passed. No work was dispatched and the ID is not reserved. Launch uses this exact plan and remains subject to live limits and tool approvals.');
  });
  el('design-launch').onclick = action(async () => {
    if (!reviewed || sourceDirty || encode(sync()) !== reviewed.editor_text) throw Error('Review the current workflow before launch.');
    const view = reviewed; el('design-launch').disabled = true; reviewed = null;
    const created = await exactApi('/workflow-design/launch', {request:view.request, preview_key:view.preview_key});
    parentSelection = {kind:'workflow', id:created.workflow_id}; await refresh();
    note('design-message', 'Workflow saved and launched. Inspect its parent and children below for progress, approvals and recovery.');
  });
})();
