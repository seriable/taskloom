"""Create private local Compose secret files; no network or model request.

Read the model credential from a named environment variable. New files are
create-only, mode 0600 on POSIX. Keep the output directory outside the checkout.
"""
import argparse
import json
import os
from pathlib import Path
import secrets
from urllib.parse import quote


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--model-key-env', required=True)
    args = parser.parse_args()
    directory = args.directory.resolve()
    checkout = Path(__file__).resolve().parents[2]
    if directory == checkout or directory.is_relative_to(checkout):
        raise ValueError('private secret directory must be outside this checkout')
    key = os.environ.get(args.model_key_env, '')
    if not key or '\n' in key or '\r' in key:
        raise ValueError('named model credential environment is missing or multiline')
    password = secrets.token_urlsafe(36)
    directory.mkdir(mode=0o700, parents=True, exist_ok=False)
    values = {'postgres-password':password,
              'database-url':'postgres://taskloom:'+quote(password, safe='')+'@database:5432/taskloom?sslmode=disable',
              'model-key':key}
    for name, value in values.items():
        fd = os.open(directory/name, os.O_CREAT|os.O_EXCL|os.O_WRONLY, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8', newline='\n') as output:
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
    print(json.dumps({'directory':str(directory), 'created_files':list(values),
                      'credential_values_logged':False, 'network_requests':0,
                      'windows_acl':'Files inherit destination ACL on Windows; restrict that private directory to the deployment operator.'}))


if __name__ == '__main__':
    main()
