/** Explicit one-attempt client. Transport errors leave dispatch uncertain. */
export class TaskloomError extends Error {
  constructor(status, kind) { super(`Taskloom ${kind}${status ? ` (HTTP ${status})` : ''}`); this.status = status; this.kind = kind; }
}
const identifier = (value, maximum = 64) => {
  if (typeof value !== 'string' || value.length<1 || value.length>maximum || !/^[A-Za-z0-9_-]+$/.test(value)) throw new Error('invalid Taskloom resource identifier');
  return encodeURIComponent(value);
};
function safeNumbers(value, seen = new Set()) {
  if (typeof value === 'number' && (!Number.isFinite(value) || Number.isInteger(value) && !Number.isSafeInteger(value))) throw new Error('JSON number exceeds finite safe precision');
  if (['undefined','bigint','function','symbol'].includes(typeof value)) throw new Error('unsupported JSON value');
  if (value && typeof value === 'object') {
    if (seen.has(value)) throw new Error('circular JSON value');
    if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) throw new Error('JSON values must be plain objects or arrays');
    seen.add(value); for (const item of (Array.isArray(value) ? value : Object.values(value))) safeNumbers(item, seen); seen.delete(value);
  }
}
function parseExact(text) {
  // Check raw numeric tokens before JSON.parse can round u64 revisions/budgets.
  for (const token of text.matchAll(/"(?:\\[\s\S]|[^"\\])*"|(-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?)/g)) {
    if (token[1] !== undefined) {
      const value = Number(token[1]);
      if (!Number.isFinite(value) || Number.isInteger(value) && !Number.isSafeInteger(value)) throw new Error('unsafe response number');
    }
  }
  return JSON.parse(text);
}
export class TaskloomClient {
  #base; #token; #timeout; #maximum;
  constructor(baseUrl, token, { timeoutMs = 30000, maxResponseBytes = 4 * 1024 * 1024 } = {}) {
    const url = new URL(baseUrl);
    const host = url.hostname.replace(/^\[|\]$/g, '');
    const local = host === 'localhost' || host === '::1' || /^127(?:\.(?:\d{1,3})){3}$/.test(host);
    if (!['https:', 'http:'].includes(url.protocol) || url.protocol === 'http:' && !local || url.username || url.password || url.search || url.hash || !url.pathname.replace(/\/$/, '').endsWith('/api')) throw new Error('use explicit HTTPS API base, or HTTP loopback, without URL credentials');
    if (typeof token !== 'string' || !token || /[\r\n]/.test(token) || !Number.isFinite(timeoutMs) || timeoutMs <= 0 || timeoutMs > 300000 || !Number.isInteger(maxResponseBytes) || maxResponseBytes < 1024 || maxResponseBytes > 16 * 1024 * 1024) throw new Error('invalid credential or limits');
    this.#base = baseUrl.replace(/\/$/, ''); this.#token = token; this.#timeout = timeoutMs; this.#maximum = maxResponseBytes;
  }
  async request(method, path, body, { signal } = {}) {
    if (!['GET', 'POST'].includes(method) || !/^\/[A-Za-z0-9_/-]+(?:\?after=[0-9]+)?$/.test(path) || path.includes('..') || path.includes('//')) throw new Error('unsupported method/path');
    if (body !== undefined) safeNumbers(body);
    const encoded = body === undefined ? undefined : JSON.stringify(body);
    if (encoded !== undefined && new TextEncoder().encode(encoded).length > 1024 * 1024) throw new Error('request exceeds 1 MiB');
    const abort = new AbortController();
    const external = () => abort.abort();
    if (signal?.aborted) throw new TaskloomError(null, 'cancelled_before_dispatch');
    signal?.addEventListener('abort', external, { once: true });
    const timer = setTimeout(external, this.#timeout);
    try {
      const response = await fetch(this.#base + path, { method, redirect: 'error', cache:'no-store', headers: { Authorization: `Bearer ${this.#token}`, 'Content-Type': 'application/json' }, body: encoded, signal: abort.signal });
      if (!response.ok) { await response.body?.cancel(); throw new TaskloomError(response.status, ({400:'invalid_request',401:'unauthenticated',403:'forbidden',404:'not_found',409:'conflict',507:'storage_limit'})[response.status] ?? 'http_error'); }
      const reader = response.body?.getReader(); const chunks = []; let total = 0;
      if (!reader) throw new TaskloomError(null, 'invalid_response_outcome_unknown');
      for (;;) { const { value, done } = await reader.read(); if (done) break; total += value.byteLength; if (total > this.#maximum) { await reader.cancel(); throw new TaskloomError(null, 'response_limit_outcome_unknown'); } chunks.push(value); }
      const bytes = new Uint8Array(total); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
      try { return parseExact(new TextDecoder('utf-8', { fatal: true }).decode(bytes)); } catch { throw new TaskloomError(null, 'invalid_response_outcome_unknown'); }
    } catch (error) { if (error instanceof TaskloomError) throw error; throw new TaskloomError(null, abort.signal.aborted ? 'cancelled_or_timeout_outcome_unknown' : 'transport_failure_outcome_unknown'); }
    finally { clearTimeout(timer); signal?.removeEventListener('abort', external); }
  }
  run(id, prompt, options) { return this.request('POST', '/runs', { id: identifier(id), prompt }, options); }
  inspect(id, options) { return this.request('GET', '/runs/' + identifier(id,128), undefined, options); }
  resume(id, options) { return this.request('POST', '/runs/' + identifier(id) + '/resume', {}, options); }
  approve(id, ticket, actor, note, options, approved = true) { return this.request('POST', '/runs/' + identifier(id) + '/approvals/' + identifier(ticket), { approved, actor, note }, options); }
  cancel(id, actor, note, options) { return this.request('POST', '/runs/' + identifier(id) + '/cancel', { actor, note }, options); }
  events(id, options, after = 0) { if (!Number.isSafeInteger(after) || after < 0) throw new Error('invalid event cursor'); return this.request('GET', '/runs/' + identifier(id,128) + '/events?after=' + after, undefined, options); }
  sessionTurn(id, turnId, prompt, options) { return this.request('POST', '/sessions/' + identifier(id) + '/turns', { turn_id: identifier(turnId), prompt }, options); }
  workflow(request, options) { return this.request('POST', '/workflows', request, options); }
  queueSubmit(task, actor, options) { return this.request('POST', '/queue/jobs', { task, actor }, options); }
  memory(options) { return this.request('GET', '/memory', undefined, options); }
  recall(text, options) { return this.request('POST', '/memory/recall', { text }, options); }
  semanticRecall(text, options) { return this.request('POST', '/memory/semantic-recall', { text }, options); }
  privateSession(id, options) { return this.request('GET', '/private/sessions/' + identifier(id), undefined, options); }
  createPrivateSession(id, options) { return this.request('POST', '/private/sessions', { id: identifier(id) }, options); }
  privateTurn(id, turnId, prompt, recall = false, options) { return this.request('POST', '/private/sessions/' + identifier(id) + (recall ? '/memory-turns' : '/turns'), { turn: identifier(turnId), prompt }, options); }
  resumePrivateTurn(id, turnId, options) { return this.request('POST', '/private/sessions/' + identifier(id) + '/resume', { turn: identifier(turnId) }, options); }
}
