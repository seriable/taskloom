"""Small synchronous Taskloom JSON client. No automatic retry or resume.

The token is provided by the host; requests cannot grant tools or identities.
Do not log serialized client configuration, prompts or unredacted responses.
"""
import json
import http.client
import math
import re
import urllib.error
import urllib.parse
import urllib.request


class TaskloomError(Exception):
    def __init__(self, status, kind):
        self.status = status
        self.kind = kind
        super().__init__(f'Taskloom {kind}' + (f' (HTTP {status})' if status else ''))


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def _identifier(value, maximum=64):
    if not isinstance(value, str) or not 1 <= len(value) <= maximum or not re.fullmatch(r'[A-Za-z0-9_-]+', value):
        raise ValueError('invalid Taskloom resource identifier')
    return urllib.parse.quote(value, safe='')


def _finite_float(value):
    number = float(value)
    if not math.isfinite(number):
        raise ValueError('non-finite response number')
    return number


def _invalid_constant(value):
    raise ValueError('invalid JSON constant')


class Client:
    """Use a local /api base or an explicit /tenants/T/projects/P/api base.

    Private tokens are never forwarded across redirects. Mutations, approval,
    resume and even reads are one attempt; a transport failure may have happened
    after dispatch and is reported as uncertain. Inspect durable state first.
    """
    def __init__(self, base_url, token, *, timeout=30, max_response_bytes=4*1024*1024):
        parsed = urllib.parse.urlsplit(base_url)
        if not parsed.hostname:
            raise ValueError('API base requires an explicit host')
        import ipaddress
        try:
            local = ipaddress.ip_address(parsed.hostname).is_loopback
        except (TypeError,ValueError):
            local = parsed.hostname == 'localhost'
        if (parsed.scheme not in ('https', 'http') or not parsed.hostname
                or parsed.scheme == 'http' and not local
                or parsed.username or parsed.password or parsed.query or parsed.fragment
                or not parsed.path.rstrip('/').endswith('/api')):
            raise ValueError('use explicit HTTPS API base, or HTTP loopback, without URL credentials')
        if (not isinstance(token, str) or not token or '\r' in token or '\n' in token
                or not 0 < timeout <= 300 or not 1024 <= max_response_bytes <= 16*1024*1024):
            raise ValueError('invalid token, timeout or response bound')
        self._base = base_url.rstrip('/')
        self._token = token
        self._timeout = timeout
        self._maximum = max_response_bytes
        self._opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())

    def __repr__(self):
        return 'TaskloomClient(credentials=<redacted>)'

    def request(self, method, path, body=None):
        if method not in ('GET', 'POST') or not re.fullmatch(r'/[A-Za-z0-9_/-]+(?:\?after=[0-9]+)?', path) or '..' in path or '//' in path:
            raise ValueError('unsupported method/path')
        encoded = None if body is None else json.dumps(body, ensure_ascii=False, allow_nan=False).encode()
        if encoded is not None and len(encoded) > 1024*1024:
            raise ValueError('request exceeds 1 MiB')
        request = urllib.request.Request(self._base+path, data=encoded, method=method,
            headers={'Authorization': 'Bearer '+self._token, 'Content-Type': 'application/json'})
        try:
            with self._opener.open(request, timeout=self._timeout) as response:
                declared = response.headers.get('Content-Length')
                if declared is not None and (not re.fullmatch(r'[0-9]{1,20}',declared)
                        or int(declared) > self._maximum):
                    raise TaskloomError(None, 'response_limit_outcome_unknown')
                raw = response.read(self._maximum+1)
                if declared is not None and len(raw) != int(declared):
                    raise TaskloomError(None, 'transport_failure_outcome_unknown')
        except urllib.error.HTTPError as error:
            # Do not copy server error content into logs or exception messages.
            status = error.code
            error.close()
            kind = {400:'invalid_request',401:'unauthenticated',403:'forbidden',404:'not_found',409:'conflict',507:'storage_limit'}.get(status,'http_error')
            raise TaskloomError(status, kind) from None
        except (urllib.error.URLError, OSError, http.client.HTTPException):
            raise TaskloomError(None, 'transport_failure_outcome_unknown') from None
        if len(raw) > self._maximum:
            raise TaskloomError(None, 'response_limit_outcome_unknown')
        try:
            return json.loads(raw,parse_float=_finite_float,parse_constant=_invalid_constant)
        except (ValueError, UnicodeError):
            raise TaskloomError(None, 'invalid_response_outcome_unknown') from None

    def run(self, run_id, prompt):
        return self.request('POST','/runs',{'id':_identifier(run_id),'prompt':prompt})

    def inspect(self, run_id):
        return self.request('GET','/runs/'+_identifier(run_id,128))

    def resume(self, run_id):
        return self.request('POST','/runs/'+_identifier(run_id)+'/resume',{})

    def approve(self, run_id, ticket, actor, note, approved=True):
        return self.request('POST','/runs/'+_identifier(run_id)+'/approvals/'+_identifier(ticket),{'approved':approved,'actor':actor,'note':note})

    def cancel(self, run_id, actor, note):
        return self.request('POST','/runs/'+_identifier(run_id)+'/cancel',{'actor':actor,'note':note})

    def events(self, run_id, after=0):
        if type(after) is not int or after < 0:
            raise ValueError('invalid event cursor')
        return self.request('GET','/runs/'+_identifier(run_id,128)+'/events?after='+str(after))

    def session_turn(self, session_id, turn_id, prompt):
        return self.request('POST','/sessions/'+_identifier(session_id)+'/turns',{'turn_id':_identifier(turn_id),'prompt':prompt})

    def workflow(self, request):
        return self.request('POST','/workflows',request)

    def queue_submit(self, task, actor):
        return self.request('POST','/queue/jobs',{'task':task,'actor':actor})

    def memory(self):
        return self.request('GET','/memory')

    def recall(self, text):
        return self.request('POST','/memory/recall',{'text':text})

    def semantic_recall(self, text):
        """Explicit paid upload of reviewed own memory to host embeddings.

        Requires the host semantic opt-in and current Submit permission.
        This never runs as a side effect of an ordinary session turn.
        """
        return self.request('POST','/memory/semantic-recall',{'text':text})

    def private_session(self, session_id):
        return self.request('GET','/private/sessions/'+_identifier(session_id))

    def create_private_session(self, session_id):
        return self.request('POST','/private/sessions',{'id':_identifier(session_id)})

    def private_turn(self, session_id, turn_id, prompt, *, recall=False):
        route='/memory-turns' if recall else '/turns'
        return self.request('POST','/private/sessions/'+_identifier(session_id)+route,{'turn':_identifier(turn_id),'prompt':prompt})

    def resume_private_turn(self, session_id, turn_id):
        return self.request('POST','/private/sessions/'+_identifier(session_id)+'/resume',{'turn':_identifier(turn_id)})
