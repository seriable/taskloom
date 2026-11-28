"""Actual native API, separate CLI processes and project generation admission.

Requires a preconfigured zero-price loopback model and explicitly owned local
PostgreSQL fixture. Kills only its own generation child; retains unknown usage.
No paid API, quality ranking, worker queue or throughput claim.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
from urllib.parse import quote, urlsplit
import uuid


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--taskloom', type=Path, required=True)
    parser.add_argument('--admin', type=Path, required=True)
    parser.add_argument('--postgres-fixture', type=Path, required=True)
    parser.add_argument('--model-log', type=Path, required=True)
    args = parser.parse_args()
    cfg = json.loads(args.config.read_text(encoding='utf-8'))
    if (urlsplit(cfg['provider']['base_url']).hostname not in {'127.0.0.1', 'localhost', '::1'}
            or not cfg.get('prices') or any(p['input_per_million'] or p['output_per_million'] for p in cfg['prices'])):
        raise ValueError('explicit zero-priced loopback model required')
    if not os.environ.get(cfg['provider']['key_env']):
        raise ValueError('configure named local model credential environment')
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=False)
    executable, admin = args.taskloom.resolve(), args.admin.resolve()
    public = json.loads((args.postgres_fixture / 'public.json').read_text())
    pg = json.loads((args.postgres_fixture / 'credentials.json').read_text())['TASKLOOM_TEST_POSTGRES_URL']
    parsed = urlsplit(pg)
    if parsed.hostname != '127.0.0.1' or parsed.port != public['loopback_port']:
        raise ValueError('owned loopback PostgreSQL fixture required')
    requests = lambda: args.model_log.read_bytes().count(b'[Request received] Model:')
    report = {'version': 1, 'completed': False, 'cases': [], 'failures': [],
              'scope': 'native SQL Application generation, simultaneous separate-process rejection, killed-process reservation, explicit quota review and cold reentry',
              'taskloom_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'admin_sha256': hashlib.sha256(admin.read_bytes()).hexdigest(), 'postgres_public_provenance': public,
              'fees': {'currency': 'USD', 'input_per_million': 0, 'output_per_million': 0, 'total_cap_micros': 0,
                       'credential_environment': cfg['provider']['key_env'], 'database_environment': 'TASKLOOM_DATABASE_URL',
                       'excluded': 'hardware/electricity/current chat; no embedding or tool service'},
              'limits': ['Short completed generation and interrupted long-output probe, no research/long-task quality acceptance.',
                         'Unknown response usage from killed request is retained; model may continue generating after connection death.',
                         '10-second production lease; no short-TTL/load/distributed-queue claim.',
                         'Trusted local administrator selects currency/prices and increases the cap; no model grants authority.']}
    for backend, url in {'sqlite': 'sqlite://' + quote(str(root / 'quota.sqlite'), safe='') + '?mode=rwc&cache=private', 'postgres': pg}.items():
        out = root / backend
        out.mkdir()
        tenant, project = 'quota-native', uuid.uuid4().hex
        env = dict(os.environ, TASKLOOM_DATABASE_URL=url)
        config = dict(cfg, state_dir=str(out / 'unused-file-state'), knowledge=False, roles=[], skills=[], memory=None,
                      output_tokens=1024, limits={'max_steps': 4, 'max_tool_calls': 0,
                          'max_duration': {'secs': 300, 'nanos': 0}, 'tool_timeout': {'secs': 5, 'nanos': 0},
                          'max_tool_result_bytes': 65536, 'max_cost_micros': None}, system='Follow the user request. No tools.',
                      database={'url_env': 'TASKLOOM_DATABASE_URL', 'tenant': tenant, 'project': project})
        save(out / 'app.json', config)
        case = {'backend': backend, 'tenant': tenant, 'project': project, 'checks': {}, 'failures': []}
        process, process_log = None, None

        def cli(parts, expected_budget=False):
            value = subprocess.run([str(executable), '--config', str(out / 'app.json'), *parts], env=env, capture_output=True, timeout=180)
            if value.returncode and not expected_budget:
                raise RuntimeError('CLI ' + parts[0] + ' failed: ' + value.stderr.decode(errors='replace')[:1200])
            data = json.loads(value.stdout)
            if value.returncode and not (expected_budget and data.get('report', {}).get('stop_reason') == 'budget'):
                raise RuntimeError('CLI ' + parts[0] + ' failed: ' + value.stderr.decode(errors='replace')[:1200])
            return data

        def quota(policy=None, revision=None, note=None):
            command = [str(admin), 'quota-get', tenant, project]
            if revision is not None:
                save(out / 'policy.json', policy)
                command = [str(admin), 'quota-set', tenant, project, str(out / 'policy.json'), str(revision), 'acceptance-operator', note]
            value = subprocess.run(command, env=env, capture_output=True, timeout=30)
            if value.returncode:
                raise RuntimeError('quota administration failed: ' + value.stderr.decode(errors='replace')[:1200])
            return json.loads(value.stdout)

        try:
            save(out / 'migration.json', cli(['database-migrate']))
            save(out / 'configured.json', quota({'currency': 'USD', 'max_cost_micros': 0, 'max_outstanding_attempts': 1}, 0, 'initial zero-price single outstanding attempt'))
            baseline = requests()
            process_log = (out / 'killed-cli.log').open('wb')
            process = subprocess.Popen([str(executable), '--config', str(out / 'app.json'), 'run', 'interrupted',
                                        'Write 100 numbered paragraphs explaining reliable task execution in detail. Continue until complete.'],
                                       env=env, stdout=process_log, stderr=process_log,
                                       creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
            for _ in range(120):
                current = quota()
                if current['usage']['in_flight_attempts'] == 1 and requests() > baseline:
                    break
                if process.poll() is not None:
                    raise RuntimeError('long generation ended before in-flight native observation')
                time.sleep(0.05)
            else:
                raise RuntimeError('in-flight observation deadline reached')
            save(out / 'before-kill-quota.json', current)
            known = requests()
            denied = cli(['run', 'simultaneous', 'Reply READY.'], expected_budget=True)
            save(out / 'simultaneous-refusal.json', denied)
            case['checks']['separate_process_refused_while_actual_native_attempt_live'] = denied['report']['stop_reason'] == 'budget' and requests() == known
            case['checks']['known_refusal_has_zero_uncertain_attempts'] = denied['report']['budget']['uncertain_attempts'] == 0
            process.kill()
            process.wait(timeout=10)
            case['killed_owned_process_id'] = process.pid
            case['killed_exit_code'] = process.returncode
            after = quota()
            save(out / 'after-kill-quota.json', after)
            case['checks']['process_death_does_not_release_quota_slot'] = after['usage']['in_flight_attempts'] == 1
            blocked = cli(['run', 'after-death', 'Reply READY.'], expected_budget=True)
            save(out / 'after-death-refusal.json', blocked)
            case['checks']['new_process_cannot_erase_dead_attempt'] = blocked['report']['stop_reason'] == 'budget' and requests() == known
            # Production TTL, no administrative theft of an active owner.
            time.sleep(10.2)
            recovered = cli(['resume', 'interrupted'], expected_budget=True)
            save(out / 'recovery.json', recovered)
            events = [event['kind']['type'] for event in recovered['report']['events']]
            current = quota()
            save(out / 'recovered-quota.json', current)
            case['checks']['explicit_recovery_stops_before_new_dispatch'] = 'recovered_model' in events and recovered['report']['stop_reason'] == 'budget' and requests() == known
            case['checks']['crash_uncertainty_retains_zero_price_slot'] = current['usage']['uncertain_attempts'] == 1 and current['usage']['in_flight_attempts'] == 0
            save(out / 'review.json', quota({'currency': 'USD', 'max_cost_micros': 0, 'max_outstanding_attempts': 2}, 1, 'explicitly permit another attempt while retaining the crashed unknown request'))
            config['output_tokens'] = 64
            save(out / 'short.json', config)
            # New config allowance is for a fresh run; terminal reentry uses
            # the original frozen config below.
            value = subprocess.run([str(executable), '--config', str(out / 'short.json'), 'run', 'after-review', 'Reply exactly READY.'],
                                   env=env, capture_output=True, timeout=180)
            completed = json.loads(value.stdout)
            save(out / 'completed.json', completed)
            if value.returncode:
                raise RuntimeError('native post-review generation failed: ' + value.stderr.decode(errors='replace')[:1200])
            case['checks']['native_generation_after_explicit_review_completed'] = completed['report']['stop_reason'] == 'completed' and completed['report']['usage']['requests'] == 1
            current = quota()
            save(out / 'final-quota.json', current)
            case['checks']['known_settlement_releases_only_new_attempt'] = current['usage']['uncertain_attempts'] == 1 and current['usage']['in_flight_attempts'] == 0 and current['usage']['committed_micros'] == 0
            known = requests()
            replay = cli(['resume', 'interrupted'], expected_budget=True)
            case['checks']['terminal_reentry_unchanged_without_api_dispatch'] = replay == recovered and requests() == known
            other = subprocess.run([str(admin), 'quota-get', tenant, 'other-' + uuid.uuid4().hex], env=env, capture_output=True, timeout=30)
            case['checks']['other_project_cannot_read_retained_accounting'] = other.returncode == 0 and json.loads(other.stdout)['usage'] is None
            case['observed_native_requests'] = requests() - baseline
            case['completed_native_usage'] = completed['report']['usage']
            case['unknown_native_response_usage_attempts'] = 1
        except Exception as error:
            case['failures'].append(str(error))
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            if process_log is not None:
                process_log.close()
        case['failures'].extend('check failed: ' + key for key, passed in case['checks'].items() if not passed)
        case['status'] = 'failed' if case['failures'] else 'passed'
        save(out / 'report.json', case)
        report['cases'].append(case)
        report['failures'].extend(backend + ': ' + failure for failure in case['failures'])
        save(root / 'report.json', report)
        print(json.dumps(case), flush=True)
    report['completed'] = True
    report['status'] = 'failed' if report['failures'] else 'passed'
    save(root / 'report.json', report)
    raise SystemExit(1 if report['failures'] else 0)


if __name__ == '__main__':
    main()
