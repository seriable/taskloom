export interface RequestOptions { signal?: AbortSignal }
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export class TaskloomError extends Error { readonly status: number | null; readonly kind: string; }
/** Response fields remain the server's versioned JSON contract; no fake typed success. */
export class TaskloomClient {
  constructor(baseUrl: string, token: string, options?: { timeoutMs?: number; maxResponseBytes?: number });
  request(method: 'GET'|'POST', path: string, body?: JsonValue, options?: RequestOptions): Promise<JsonValue>;
  run(id: string, prompt: string, options?: RequestOptions): Promise<JsonValue>;
  inspect(id: string, options?: RequestOptions): Promise<JsonValue>;
  resume(id: string, options?: RequestOptions): Promise<JsonValue>;
  approve(id: string, ticket: string, actor: string, note: string, options?: RequestOptions, approved?: boolean): Promise<JsonValue>;
  cancel(id: string, actor: string, note: string, options?: RequestOptions): Promise<JsonValue>;
  events(id: string, options?: RequestOptions, after?: number): Promise<JsonValue>;
  sessionTurn(id: string, turnId: string, prompt: string, options?: RequestOptions): Promise<JsonValue>;
  workflow(request: JsonValue, options?: RequestOptions): Promise<JsonValue>;
  queueSubmit(task: JsonValue, actor: string, options?: RequestOptions): Promise<JsonValue>;
  memory(options?: RequestOptions): Promise<JsonValue>;
  recall(text: string, options?: RequestOptions): Promise<JsonValue>;
  /** Explicit paid upload of reviewed own memory; host opt-in and Submit required. */
  semanticRecall(text: string, options?: RequestOptions): Promise<JsonValue>;
  privateSession(id: string, options?: RequestOptions): Promise<JsonValue>;
  createPrivateSession(id: string, options?: RequestOptions): Promise<JsonValue>;
  privateTurn(id: string, turnId: string, prompt: string, recall?: boolean, options?: RequestOptions): Promise<JsonValue>;
  resumePrivateTurn(id: string, turnId: string, options?: RequestOptions): Promise<JsonValue>;
}
