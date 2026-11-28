import { TaskloomClient, TaskloomError, type JsonValue } from './index.js';
const client = new TaskloomClient('http://127.0.0.1:8080/api', 'configured-on-host');
const controller = new AbortController();
const result: Promise<JsonValue> = client.inspect('example', { signal: controller.signal });
client.approve('example', 'ticket', 'operator', 'checked', undefined, false);
client.events('example', undefined, 200);
void result.catch((error: TaskloomError) => { const status: number | null = error.status; return status; });
