"""Conservative managed-Docker PostgreSQL DR, with an independent SQL witness.

This is an operator tool, not automatic failover. Only explicitly owned local
postgres_fixture deployments and exact managed Docker workers are supported.
Every output/backup/config is PRIVATE. No cloud request or automatic job replay.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import time
from urllib.parse import urlsplit
import uuid


DB_LABEL = 'com.taskloom.database-acceptance'
LABEL = 'com.taskloom.dr.deployment'
CATEGORIES = {'configuration', 'state', 'signed_registry', 'qdrant_journal',
              'rerank_ledger', 'source_refs', 'secret_refs', 'secrets', 'external_effects'}
MAX_FILES = 4096
MAX_FILE = 64 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024


class Refused(RuntimeError):
    pass


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(',', ':')).encode()


def sha(value):
    return hashlib.sha256(value).hexdigest()


def private_save(path, value):
    path = Path(path)
    with path.open('xb') as stream:
        os.chmod(path, 0o600)
        stream.write(canonical(value) + b'\n')
        stream.flush()
        os.fsync(stream.fileno())


def flush_tree(root):
    """Flush backup file contents; directory fsync where the OS supports it."""
    root = Path(root)
    for path in root.rglob('*'):
        if path.is_file():
            # Windows CRT _commit rejects read-only descriptors. These are
            # private, newly created backup/restore copies, never source files.
            if os.name == 'nt':
                os.chmod(path, 0o600)
            with path.open('r+b' if os.name == 'nt' else 'rb') as stream:
                os.fsync(stream.fileno())
    if os.name != 'nt':
        for path in [*sorted((p for p in root.rglob('*') if p.is_dir()), reverse=True), root]:
            descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch('[a-z][a-z0-9_]{0,62}', value):
        raise Refused('invalid bounded PostgreSQL identifier')
    return '"' + value + '"'


def literal(value):
    # SQL is sent over stdin, never interpolated into a shell command or argv.
    if '\0' in value or len(value.encode()) > 1024 * 1024:
        raise Refused('invalid SQL literal')
    return "'" + value.replace("'", "''") + "'"


def docker(*parts, data=None, timeout=60, environment=None):
    try:
        result = subprocess.run(['docker', *map(str, parts)], input=data,
                                capture_output=True, timeout=timeout, env=environment)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise Refused('Docker operation unavailable or acknowledgement unknown') from error
    if result.returncode or len(result.stdout) > MAX_TOTAL:
        # PostgreSQL, Docker and application errors can contain secrets/data.
        raise Refused('owned Docker operation failed; inspect private deployment locally')
    return result.stdout


class Database:
    def __init__(self, fixture):
        self.fixture = Path(fixture).resolve()
        self.owner = json.loads((self.fixture / 'ownership.json').read_bytes())
        self.cid = self.owner['container_id']
        if not re.fullmatch('[a-f0-9]{64}', self.cid):
            raise Refused('invalid explicit database identity')
        self.verify()

    def verify(self):
        state = json.loads(docker('inspect', self.cid, '--format', '{{json .}}'))
        if (state['Config']['Labels'].get(DB_LABEL) != self.owner['nonce']
                or state['Image'] != self.owner['image_id'] or not state['State']['Running'] or state['State']['Paused']):
            raise Refused('owned database unavailable or identity changed')

    def sql(self, sql, database='taskloom_accept'):
        identifier(database)
        self.verify()
        raw = docker('exec', '--interactive', self.cid, 'psql', '-X', '-qAt',
                     '-v', 'ON_ERROR_STOP=1', '-U', 'taskloom_accept', '-d', database,
                     data=("SET standard_conforming_strings=on; SET synchronous_commit=on; "
                           "SET lock_timeout='5s'; SET statement_timeout='30s';\n" + sql).encode())
        return raw.decode('utf-8').strip()

    def value(self, sql, database='taskloom_accept'):
        return json.loads(self.sql(sql, database))

    def role(self, name):
        identifier(name)
        return self.value('SELECT row_to_json(r) FROM (SELECT rolname,rolcanlogin,rolsuper,'
                          'rolcreatedb,rolcreaterole,rolreplication,rolbypassrls,rolinherit,'
                          '(SELECT count(*) FROM pg_auth_members m WHERE m.member=roles.oid OR '
                          'm.roleid=roles.oid) memberships FROM pg_roles roles WHERE rolname=' + literal(name) + ') r;')

    def safe_role(self, name):
        role = self.role(name)
        if not role or any(role[key] for key in ['rolsuper', 'rolcreatedb', 'rolcreaterole',
                                                 'rolreplication', 'rolbypassrls', 'rolinherit', 'memberships']):
            raise Refused('dedicated non-inheriting non-privileged application role required')
        return role

    def fence(self, name, database):
        self.safe_role(name)
        self.sole_application_login(name, database)
        self.sql('ALTER ROLE ' + identifier(name) + ' NOLOGIN;')
        self.sql('SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename='
                 + literal(name) + ' AND pid<>pg_backend_pid();')
        count = int(self.sql('SELECT count(*) FROM pg_stat_activity WHERE usename=' + literal(name) + ';'))
        if self.safe_role(name)['rolcanlogin'] or count:
            raise Refused('source SQL fencing proof incomplete')
        # A different writer or administrator connection prevents quiescence.
        if int(self.sql('SELECT count(*) FROM pg_stat_activity WHERE datname=' + literal(database)
                        + " AND backend_type='client backend' AND pid<>pg_backend_pid();", database)):
            raise Refused('unmanaged source database connection prevents checkpoint')

    def sole_application_login(self, name, database):
        identifier(database)
        # The owned fixture administrator is a trusted control-plane identity.
        # Another login-capable SQL principal makes the runtime fence incomplete.
        if int(self.sql('SELECT count(*) FROM pg_roles WHERE rolcanlogin AND rolname NOT IN ('
                        + literal(name) + ",'taskloom_accept') AND has_database_privilege(oid,"
                        + literal(database) + ",'CONNECT');")):
            raise Refused('unmanaged database login prevents single-writer fencing proof')

    def fingerprint(self, database):
        # This conservative profile supports only ordinary public tables and
        # sequences. pg_dump includes schema/index/constraint/sequence state;
        # hashes of every actual table cover all rows, not a hard-coded five.
        user_namespace = "n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp_%'"
        checks = ["SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace "
                  "WHERE n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' "
                  "AND (n.nspname<>'public' OR c.relkind NOT IN ('r','i','S','t'))",
                  "SELECT count(*) FROM pg_extension WHERE extname<>'plpgsql'",
                  'SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE ' + user_namespace,
                  'SELECT count(*) FROM pg_namespace n WHERE ' + user_namespace + " AND n.nspname<>'public'"]
        # Ordinary table row/composite types and their automatically generated
        # array types are allowed; independent domains/enums/ranges/base shells
        # are not part of this intentionally small restore profile.
        checks.append('SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE '
            + user_namespace + " AND NOT (t.typtype='c' AND EXISTS(SELECT 1 FROM pg_class c WHERE c.oid=t.typrelid AND c.relkind='r') "
            "OR t.typtype='b' AND t.typelem<>0 AND EXISTS(SELECT 1 FROM pg_type e JOIN pg_class c ON c.oid=e.typrelid WHERE e.oid=t.typelem AND e.typtype='c' AND c.relkind='r'))")
        for catalog, namespace_column in [('pg_collation', 'collnamespace'), ('pg_conversion', 'connamespace'),
                                           ('pg_operator', 'oprnamespace'), ('pg_opclass', 'opcnamespace'),
                                           ('pg_opfamily', 'opfnamespace'), ('pg_ts_config', 'cfgnamespace'),
                                           ('pg_ts_dict', 'dictnamespace')]:
            checks.append('SELECT count(*) FROM ' + catalog + ' p JOIN pg_namespace n ON n.oid=p.'
                          + namespace_column + ' WHERE ' + user_namespace)
        checks.extend(['SELECT count(*) FROM ' + catalog for catalog in [
            'pg_largeobject_metadata', 'pg_event_trigger', 'pg_publication', 'pg_subscription',
            'pg_foreign_server', 'pg_foreign_data_wrapper', 'pg_db_role_setting']])
        checks.extend(["SELECT count(*) FROM pg_language WHERE lanname NOT IN ('internal','c','sql','plpgsql')",
            "SELECT count(*) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND NOT t.tgisinternal",
            "SELECT count(*) FROM pg_rewrite r JOIN pg_class c ON c.oid=r.ev_class JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public'",
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relrowsecurity"])
        unsupported = int(self.sql('SELECT ' + '+'.join('(' + query + ')' for query in checks) + ';', database))
        if unsupported:
            raise Refused('database catalog outside managed DR v1 profile')
        if int(self.sql('SELECT pg_database_size(current_database());', database)) > MAX_TOTAL:
            raise Refused('managed database size bound exceeded')
        schema = docker('exec', self.cid, 'pg_dump', '-U', 'taskloom_accept', '-d', database,
                        '--schema-only', '--no-owner', '--no-privileges')
        # PostgreSQL security backports generate a fresh psql restrict nonce in
        # each dump. Normalize only that paired command, never schema content.
        schema = re.sub(rb'(?m)^\\(un)?restrict [A-Za-z0-9]+\r?$', rb'\\\1restrict [dump nonce]', schema)
        tables = self.value("SELECT coalesce(json_agg(tablename ORDER BY tablename),'[]') FROM pg_tables WHERE schemaname='public';", database)
        values = {}
        logical_bytes = 0
        for table in tables:
            name = identifier(table)
            metadata = self.value("SELECT json_build_object('rows',count(*),'bytes',coalesce(sum(octet_length(row_to_json(t)::text)),0)) FROM public." + name + ' t;', database)
            logical_bytes += metadata['bytes']
            if logical_bytes > MAX_TOTAL:
                raise Refused('logical table snapshot bound exceeded')
            raw = self.sql('SELECT row_to_json(t)::text FROM public.' + name + ' t ORDER BY row_to_json(t)::text COLLATE "C";', database).encode()
            values[table] = {'sha256': sha(raw), 'rows': metadata['rows']}
        sequences = self.value("SELECT coalesce(json_agg(sequencename ORDER BY sequencename),'[]') FROM pg_sequences WHERE schemaname='public';", database)
        seq = {name: self.sql("SELECT json_build_object('last_value',last_value,'is_called',is_called) FROM public." + identifier(name) + ';', database) for name in sequences}
        settings = self.value('SELECT row_to_json(d) FROM (SELECT encoding,datcollate,datctype,datlocprovider,daticulocale FROM pg_database WHERE datname=current_database()) d;', database)
        return {'schema_sha256': sha(schema), 'tables': values, 'sequences': seq, 'database_settings': settings}


def files(root, inventory):
    """Full regular-file root; no unlisted external path or special file."""
    root = Path(root).resolve()
    if set(inventory) != CATEGORIES:
        raise Refused('all independent asset categories must be explicitly inventoried')
    prefixes = []
    for category, declaration in inventory.items():
        if not isinstance(declaration, dict) or set(declaration) != {'paths', 'absent_reason'}:
            raise Refused('invalid category declaration')
        if bool(declaration['paths']) == bool(declaration['absent_reason']):
            raise Refused('category needs paths or explicit absence reason')
        for part in declaration['paths']:
            path = Path(part)
            if path.is_absolute() or '\\' in part or '..' in path.parts or part in ('', '.'):
                raise Refused('assets must remain under the complete managed root')
            if not (root / path).exists():
                raise Refused('declared asset absent')
            prefixes.append((path.as_posix(), category))
    result = {}
    total = 0
    for directory, directories, filenames in os.walk(root, followlinks=False):
        for name in directories + filenames:
            path = Path(directory) / name
            info = path.lstat()
            if (stat.S_ISLNK(info.st_mode)
                    or getattr(info, 'st_file_attributes', 0) & getattr(stat, 'FILE_ATTRIBUTE_REPARSE_POINT', 0)
                    or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode))):
                raise Refused('symlink or special asset refused')
            if not stat.S_ISREG(info.st_mode):
                continue
            relative = path.relative_to(root).as_posix()
            matched = [category for prefix, category in prefixes if relative == prefix or relative.startswith(prefix + '/')]
            if len(matched) != 1:
                raise Refused('every managed asset must have exactly one inventory category')
            if info.st_size > MAX_FILE:
                raise Refused('asset per-file bound exceeded')
            raw = path.read_bytes()
            total += len(raw)
            result[relative] = {'sha256': sha(raw), 'bytes': len(raw), 'category': matched[0]}
            if len(result) > MAX_FILES or total > MAX_TOTAL:
                raise Refused('asset snapshot bound exceeded')
    return result


def runtime_material(root):
    """Explicit selected runtime secrets; never inherit the host's whole env."""
    root = Path(root)
    configuration = json.loads((root / 'config' / 'project.json').read_bytes())
    environment = json.loads((root / 'secrets' / 'runtime-env.json').read_bytes())
    references = json.loads((root / 'secret_refs' / 'runtime.json').read_bytes())
    if (not isinstance(environment, dict) or len(environment) > 64
            or references != {'version': 1, 'names': sorted([*environment, 'TASKLOOM_DR_DATABASE_URL'])}):
        raise Refused('explicit complete runtime secret reference/value map required')
    blocked = {'PATH', 'HOME', 'USER', 'SHELL', 'BASH_ENV', 'ENV', 'IFS', 'LANG', 'LC_ALL', 'RUST_LOG',
               'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'NO_PROXY', 'TASKLOOM_DR_DATABASE_URL'}
    for name, value in environment.items():
        if (not re.fullmatch('[A-Z][A-Z0-9_]{0,127}', name) or name in blocked
                or name.startswith(('LD_', 'DYLD_', 'DOCKER_', 'PYTHON', 'NODE_', 'RUST_'))
                or not isinstance(value, str) or '\0' in value or not 1 <= len(value.encode()) <= 16384):
            raise Refused('invalid selected secret environment')
    found = set()

    def visit(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key.endswith('_env') and item is not None:
                    if not isinstance(item, str):
                        raise Refused('invalid configuration environment reference')
                    found.add(item)
                visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)
    visit(configuration)
    for item in configuration.get('mcp_stdio', []):
        found.update(item.get('environment', []))
    if not found <= set(references['names']):
        raise Refused('configuration secret reference omitted from private backup')
    if (configuration.get('database', {}).get('url_env') != 'TASKLOOM_DR_DATABASE_URL'
            or configuration.get('state_dir') != '../state'):
        raise Refused('managed v1 requires stable virtual config/state/database environment binding')
    return environment


def deployment_material(root, region, database, role):
    secret = json.loads((Path(root) / 'secrets' / 'deployment.json').read_bytes())
    if (secret.get('role') != role or secret.get('database') != database
            or not isinstance(secret.get('url'), str) or len(secret['url']) > 4096):
        raise Refused('epoch deployment credential mismatch')
    try:
        address = urlsplit(secret['url'])
        valid = (address.scheme == 'postgres' and address.username == role
                 and address.hostname == region and address.port == 5432
                 and address.path == '/' + database and bool(address.password)
                 and address.query == 'sslmode=disable' and not address.fragment)
    except ValueError:
        valid = False
    if not valid:
        raise Refused('runtime SQL principal/region/database must match the fenced deployment')
    return secret


class Controller:
    def __init__(self, specification):
        self.spec_path = Path(specification).resolve()
        self.spec = json.loads(self.spec_path.read_bytes())
        self.nonce = self.spec['deployment']
        if not re.fullmatch('[a-f0-9]{32}', self.nonce):
            raise Refused('invalid managed deployment identity')
        self.witness = Database(self.spec['witness_fixture'])
        self.regions = {key: Database(value['postgres_fixture']) for key, value in self.spec['regions'].items()}
        if set(self.regions) != {'a', 'b'} or len({db.cid for db in [self.witness, *self.regions.values()]}) != 3:
            raise Refused('independent third PostgreSQL witness and two regional servers required')
        if len({db.sql('SELECT system_identifier FROM pg_control_system();') for db in [self.witness, *self.regions.values()]}) != 3:
            raise Refused('independently initialized physical database servers required')
        for db in [self.witness, *self.regions.values()]:
            if db.sql('SHOW fsync;') != 'on' or db.sql('SHOW full_page_writes;') != 'on':
                raise Refused('durable PostgreSQL server settings required')
        self.image = self.spec['worker_image_id']
        if not re.fullmatch('sha256:[a-f0-9]{64}', self.image):
            raise Refused('immutable explicit worker image required')
        image = json.loads(docker('image', 'inspect', self.image, '--format', '{{json .}}'))
        if image['Id'] != self.image:
            raise Refused('worker image identity changed')
        self.network = self.spec['network_id']
        network = json.loads(docker('network', 'inspect', self.network, '--format', '{{json .}}'))
        if network['Labels'].get(LABEL) != self.nonce or not network['Internal']:
            raise Refused('exact owned internal worker network required')

    def state(self):
        value = self.witness.value('SELECT body FROM tl_dr_authority WHERE singleton;')
        if value.get('deployment') != self.nonce:
            raise Refused('witness authority belongs to another managed deployment')
        return value

    def cas(self, old, new, event):
        # Equality CAS and its audit insertion commit in one SQL transaction.
        sql = ('WITH updated AS (UPDATE tl_dr_authority SET body=' + literal(canonical(new).decode())
               + '::jsonb WHERE singleton AND body=' + literal(canonical(old).decode())
               + '::jsonb RETURNING body), audit AS (INSERT INTO tl_dr_audit(epoch,event,body) SELECT '
               + str(new['epoch']) + ',' + literal(event) + ',body FROM updated RETURNING 1) SELECT count(*) FROM audit;')
        if self.witness.sql(sql) != '1':
            raise Refused('witness CAS refused; stale or concurrent operation')

    def initialize(self, region, database, role, assets):
        identifier(database)
        self.regions[region].safe_role(role)
        self.regions[region].sole_application_login(role, database)
        if not self.regions[region].role(role)['rolcanlogin']:
            raise Refused('initial application role must be explicitly enabled')
        files(assets, self.spec['inventory'])
        runtime_material(assets)
        deployment_material(assets, region, database, role)
        initial = {'deployment': self.nonce, 'epoch': 1, 'status': 'active', 'region': region, 'database': database,
                   'role': role, 'assets': str(Path(assets).resolve()), 'ready': True, 'operation': None}
        self.witness.sql('BEGIN; CREATE TABLE tl_dr_authority(singleton boolean PRIMARY KEY CHECK(singleton),body jsonb NOT NULL); '
            'CREATE TABLE tl_dr_audit(sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,epoch bigint NOT NULL,event text NOT NULL,body jsonb NOT NULL); '
            'INSERT INTO tl_dr_authority VALUES(true,' + literal(canonical(initial).decode()) + '::jsonb); '
            'INSERT INTO tl_dr_audit(epoch,event,body) VALUES(1,\'initialize\',' + literal(canonical(initial).decode()) + '::jsonb); COMMIT;')
        return initial

    def containers(self):
        raw = docker('ps', '--all', '--no-trunc', '--filter', 'label=' + LABEL + '=' + self.nonce, '--format', '{{.ID}}')
        result = []
        for cid in raw.decode().splitlines():
            if not re.fullmatch('[a-f0-9]{64}', cid):
                raise Refused('invalid worker identity')
            item = json.loads(docker('inspect', cid, '--format', '{{json .}}'))
            labels = item['Config']['Labels']
            if (item['Image'] != self.image or labels.get(LABEL) != self.nonce
                    or labels.get('com.taskloom.dr.kind') != 'worker'):
                raise Refused('unknown managed container identity')
            result.append(item)
        return result

    def stop_workers(self, source):
        stopped = []
        source_credential = deployment_material(source['assets'], source['region'], source['database'], source['role'])
        for item in self.containers():
            labels = item['Config']['Labels']
            if labels['com.taskloom.dr.region'] != source['region']:
                if item['State']['Running']:
                    raise Refused('different regional worker is already running')
                continue
            if item['State']['Running']:
                selected = [entry.split('=', 1)[1] for entry in item['Config']['Env']
                            if entry.startswith('TASKLOOM_DR_DATABASE_URL=')]
                if (labels.get('com.taskloom.dr.epoch') != str(source['epoch'])
                        or selected != [source_credential['url']]):
                    raise Refused('running source worker carries an unverified SQL principal/epoch')
                docker('stop', '--time', '15', item['Id'], timeout=25)
            state = json.loads(docker('inspect', item['Id'], '--format', '{{json .State}}'))
            if state['Running']:
                raise Refused('managed source worker stopping incomplete')
            stopped.append(item['Id'])
        return stopped

    def runtime(self, state, *command, worker=False, probe=False, probe_token=None):
        # The private epoch credential is not part of the stable application
        # config/queue binding. It is mounted/selected by trusted host only.
        current = self.state()
        allowed_probe = probe and ((command == ('queue',) and not worker) or
                                   (command == ('serve', '127.0.0.1:18080') and worker and
                                    isinstance(probe_token, str) and re.fullmatch('[a-f0-9]{64}', probe_token)))
        if (current != state or state['status'] != 'active'
                or (not state['ready'] and not allowed_probe)):
            raise Refused('only ready witness Active permits managed process start')
        secret = deployment_material(state['assets'], state['region'], state['database'], state['role'])
        selected = runtime_material(state['assets'])
        environment = dict(os.environ, **selected, TASKLOOM_DR_DATABASE_URL=secret['url'])
        if probe_token:
            if not allowed_probe:
                raise Refused('invalid private binding probe')
            environment['TASKLOOM_API_TOKEN'] = probe_token
        name = 'tl-dr-' + uuid.uuid4().hex
        parts = ['docker', 'run', '--pull', 'never', '--name', name,
                 '--label', LABEL + '=' + self.nonce, '--label', 'com.taskloom.dr.kind=worker',
                 '--label', 'com.taskloom.dr.region=' + state['region'],
                 '--label', 'com.taskloom.dr.epoch=' + str(state['epoch']),
                 '--network', self.network, '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges',
                 '--mount', 'type=bind,source=' + state['assets'] + ',target=/managed',
                 '--env', 'TASKLOOM_DR_DATABASE_URL', '--entrypoint', '/usr/local/bin/taskloom']
        for name in sorted(selected):
            parts += ['--env', name]
        if probe_token:
            parts += ['--env', 'TASKLOOM_API_TOKEN']
        if worker:
            parts += ['--detach']
        else:
            parts += ['--rm']
        parts += [self.image, '--config', '/managed/config/project.json', *command]
        try:
            result = subprocess.run(parts, env=environment, capture_output=True, timeout=45)
        except (OSError, subprocess.TimeoutExpired) as error:
            raise Refused('managed start acknowledgement unknown; inspect exact deployment labels') from error
        if result.returncode:
            raise Refused('managed worker/cold read failed; private local diagnostics required')
        return result.stdout.decode().strip()

    def probe_binding(self, state):
        """Actual Application::queue_binding via an ephemeral loopback API.

        CLI `queue` is intentionally a cold SQL read and does not instantiate
        Application. It alone cannot verify the new process' computed binding.
        This server has no automatic SQL worker and receives only one GET.
        """
        token = uuid.uuid4().hex + uuid.uuid4().hex
        cid = self.runtime(state, 'serve', '127.0.0.1:18080', worker=True, probe=True, probe_token=token)
        if not re.fullmatch('[a-f0-9]{64}', cid):
            raise Refused('binding probe container acknowledgement unknown')
        try:
            deadline = time.monotonic() + 40
            script = ("exec 3<>/dev/tcp/127.0.0.1/18080; "
                      "printf 'GET /api/queue HTTP/1.1\\r\\nHost: localhost\\r\\nAuthorization: Bearer %s\\r\\nConnection: close\\r\\n\\r\\n' "
                      '"$TASKLOOM_API_TOKEN" >&3; cat <&3')
            value = None
            while time.monotonic() < deadline:
                try:
                    raw = docker('exec', '--env', 'TASKLOOM_API_TOKEN', cid, '/bin/bash', '-c', script,
                                 environment=dict(os.environ, TASKLOOM_API_TOKEN=token), timeout=5)
                    header, body = raw.split(b'\r\n\r\n', 1)
                    if not header.startswith(b'HTTP/1.1 200 '):
                        raise Refused('actual queue binding API refused')
                    value = json.loads(body)
                    break
                except (Refused, ValueError, KeyError):
                    state_now = json.loads(docker('inspect', cid, '--format', '{{json .State}}'))
                    if not state_now['Running']:
                        raise Refused('actual Application binding probe process failed')
                    time.sleep(0.2)
            if value is None:
                raise Refused('actual queue binding probe deadline')
            binding = value['host_binding']
            frozen = value['policy']['host_binding']
            if not isinstance(binding, str) or binding != frozen:
                raise Refused('restored configuration differs from frozen queue binding')
            return binding
        finally:
            item = json.loads(docker('inspect', cid, '--format', '{{json .}}'))
            if item['Image'] != self.image or item['Config']['Labels'].get(LABEL) != self.nonce:
                raise Refused('binding probe ownership changed; refuse cleanup')
            docker('rm', '--force', cid)

    def promote(self, target, output, failpoint=None):
        """No compensation/replay. A failed transition is poisoned Unknown."""
        old = self.state()
        if old['status'] != 'active' or not old['ready'] or target == old['region'] or target not in self.regions:
            raise Refused('promotion requires a ready Active and different region')
        output = Path(output).resolve()
        source_root = Path(old['assets']).resolve()
        if output == source_root or source_root in output.parents or output in source_root.parents:
            raise Refused('operation output must be independent of managed source assets')
        output.mkdir(parents=True, exist_ok=False)
        os.chmod(output, 0o700)
        operation = uuid.uuid4().hex
        pending = dict(old, epoch=old['epoch'] + 1, status='prepare', ready=False,
                       operation={'id': operation, 'source': old['region'], 'target': target})
        started = False
        try:
            # Persist Prepare before fencing, copying, restore or enabling.
            self.cas(old, pending, 'prepare')
            started = True
            source_db = self.regions[old['region']]
            deployment_material(old['assets'], old['region'], old['database'], old['role'])
            source_db.fence(old['role'], old['database'])
            stopped = self.stop_workers(old)
            source_db.fence(old['role'], old['database'])
            deadline = time.monotonic() + 330
            while int(source_db.sql('SELECT count(*) FROM taskloom_leases WHERE expires_ms>'
                                   '(extract(epoch FROM clock_timestamp())*1000)::bigint;', old['database'])):
                if time.monotonic() > deadline:
                    raise Refused('source database-clock lease expiry deadline')
                time.sleep(0.2)
            source_assets = Path(old['assets'])
            before = files(source_assets, self.spec['inventory'])
            runtime_material(source_assets)
            backup = output / 'sealed-assets'
            shutil.copytree(source_assets, backup, symlinks=False)
            if files(source_assets, self.spec['inventory']) != before or files(backup, self.spec['inventory']) != before:
                raise Refused('managed assets changed during checkpoint')
            flush_tree(backup)
            fingerprint = source_db.fingerprint(old['database'])
            dump = source_db.cid
            raw = docker('exec', dump, 'pg_dump', '-U', 'taskloom_accept', '-d', old['database'],
                         '-Fc', '--no-owner', '--no-privileges')
            if len(raw) > MAX_TOTAL:
                raise Refused('full database dump bound exceeded')
            with (output / 'database.dump').open('xb') as stream:
                os.chmod(output / 'database.dump', 0o600)
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
            if source_db.fingerprint(old['database']) != fingerprint or files(source_assets, self.spec['inventory']) != before:
                raise Refused('source changed while sealing')
            manifest = {'version': 1, 'epoch': pending['epoch'], 'operation': operation,
                        'assets': before, 'database': fingerprint, 'dump_sha256': sha(raw)}
            private_save(output / 'private-manifest.json', manifest)
            flush_tree(output)
            seal = sha(canonical(manifest))
            if failpoint:
                failpoint('sealed', pending)
            target_db = self.regions[target]
            database = 'tl_dr_' + operation[:24]
            role = 'tl_dr_e' + str(pending['epoch']) + '_' + operation[:16]
            password = uuid.uuid4().hex + uuid.uuid4().hex
            # A fresh role/database on every epoch; never reactivate historical
            # credentials, including reverse promotion to the original region.
            target_db.sql('CREATE ROLE ' + identifier(role) + ' NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD ' + literal(password) + ';')
            target_db.sql('CREATE DATABASE ' + identifier(database) + ' TEMPLATE template0;')
            target_db.sql('REVOKE ALL ON DATABASE ' + identifier(database) + ' FROM PUBLIC;')
            docker('exec', '--interactive', target_db.cid, 'pg_restore', '--exit-on-error', '--single-transaction',
                   '--no-owner', '--no-privileges', '-U', 'taskloom_accept', '-d', database, data=raw)
            if target_db.fingerprint(database) != fingerprint or target_db.safe_role(role)['rolcanlogin']:
                raise Refused('restored database/schema/data verification failed')
            restored = output / 'restored-assets'
            shutil.copytree(backup, restored)
            if files(restored, self.spec['inventory']) != before:
                raise Refused('restored asset seal mismatch')
            url = 'postgres://' + role + ':' + password + '@' + target + ':5432/' + database + '?sslmode=disable'
            credential = restored / 'secrets' / 'deployment.json'
            credential.unlink()
            private_save(credential, {'role': role, 'database': database, 'url': url})
            derived = files(restored, self.spec['inventory'])
            if (set(derived) != set(before) or any(derived[key] != before[key] for key in before if key != 'secrets/deployment.json')):
                raise Refused('unexpected target asset change during epoch credential derivation')
            private_save(output / 'private-restored-manifest.json', derived)
            flush_tree(restored)
            derived_hash = sha(canonical(derived))
            # Application is DML-only. Schema remains administrator-owned and
            # migrations are a separate explicit operator action.
            target_db.sql('GRANT CONNECT ON DATABASE ' + identifier(database) + ' TO ' + identifier(role) + ';')
            target_db.sql('GRANT USAGE ON SCHEMA public TO ' + identifier(role) + '; GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA public TO '
                          + identifier(role) + '; GRANT USAGE,SELECT,UPDATE ON ALL SEQUENCES IN SCHEMA public TO ' + identifier(role) + ';', database)
            active = {'deployment': self.nonce, 'epoch': pending['epoch'], 'status': 'active', 'region': target,
                      'database': database, 'role': role, 'assets': str(restored), 'ready': False,
                      'operation': dict(pending['operation'], seal_sha256=seal, restored_assets_sha256=derived_hash)}
            self.cas(pending, active, 'activate_reserved_not_ready')
            if failpoint:
                failpoint('active_before_login', active)
            target_db.sql('ALTER ROLE ' + identifier(role) + ' LOGIN;')
            # Cold read initializes actual AppStores and reads real queue.
            # It does not claim a job or resume unknown external outcomes.
            cold = self.runtime(active, 'queue', probe=True)
            json.loads(cold)
            binding = self.probe_binding(active)
            ready = dict(active, ready=True)
            self.cas(active, ready, 'active_ready')
            if failpoint:
                failpoint('active_lost_ack', ready)
            receipt = {'version': 1, 'status': 'passed', 'epoch': ready['epoch'],
                       'source_region': old['region'], 'target_region': target,
                       'seal_sha256': seal, 'dump_sha256': sha(raw),
                       'restored_assets_sha256': derived_hash,
                       'asset_files': len(before), 'asset_categories': {key: sum(item['category'] == key for item in before.values()) for key in sorted(CATEGORIES)},
                       'sql_tables': len(fingerprint['tables']), 'managed_workers_stopped': len(stopped),
                       'cold_queue_sha256': sha(cold.encode()), 'cloud_model_requests': 0}
            receipt['actual_frozen_queue_binding'] = binding
            private_save(output / 'receipt.json', receipt)
            return ready, receipt
        except BaseException:
            # Even a failed CAS acknowledgement may have committed. Re-read
            # authoritative state; only this exact operation may poison it.
            try:
                current = self.state()
                if current.get('operation', {}).get('id') == operation:
                    self.cas(current, dict(current, status='unknown', ready=False), 'unknown_manual_reconciliation_required')
            except BaseException:
                pass
            # Never reenable the source, retry a dispatch, start a worker or
            # delete evidence to make an uncertain transition look successful.
            private_save(output / 'refused.json', {'status': 'unknown' if started else 'refused_or_witness_ack_unknown',
                                                   'epoch': pending['epoch'], 'manual_reconciliation_required': True})
            raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--spec', type=Path, required=True)
    sub = parser.add_subparsers(dest='action', required=True)
    initial = sub.add_parser('initialize')
    initial.add_argument('--region', choices=['a', 'b'], required=True)
    initial.add_argument('--database', required=True)
    initial.add_argument('--role', required=True)
    initial.add_argument('--assets', type=Path, required=True)
    promotion = sub.add_parser('promote')
    promotion.add_argument('--target', choices=['a', 'b'], required=True)
    promotion.add_argument('--out', type=Path, required=True)
    sub.add_parser('inspect')
    sub.add_parser('start-worker')
    args = parser.parse_args()
    try:
        controller = Controller(args.spec)
        if args.action == 'initialize':
            value = controller.initialize(args.region, args.database, args.role, args.assets)
        elif args.action == 'promote':
            _, value = controller.promote(args.target, args.out)
        elif args.action == 'start-worker':
            value = {'managed_worker_id': controller.runtime(controller.state(), 'worker', 'managed-dr', worker=True)}
        else:
            state = controller.state()
            value = {key: state[key] for key in ['epoch', 'status', 'region', 'ready']}
        # Public display contains no names, raw SQL, secrets or private paths.
        print(json.dumps({key: value[key] for key in ['epoch', 'status', 'region', 'ready'] if key in value}
                         if args.action == 'initialize' else value))
    except Exception:
        raise SystemExit('Managed DR refused or outcome unknown; inspect private witness/operation locally. No compensation or replay was attempted.')


if __name__ == '__main__':
    main()
